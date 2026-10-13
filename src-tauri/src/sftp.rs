use crate::connections::{build_ssh_args, SshMode, SshProfile};
use russh_sftp::{
    client::{error::Error as SftpError, RawSftpSession},
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    process::{Child, ChildStdin, Command},
    task::JoinHandle,
    time::timeout,
};

const CHUNK: usize = 32 * 1024;
const MAX_PACKET: usize = 256 * 1024;
const MAX_TRANSFER: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const MAX_INVENTORY: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<u32>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteListing {
    pub path: String,
    pub entries: Vec<RemoteEntry>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferResult {
    pub bytes: u64,
    pub local_path: String,
    pub remote_path: String,
}

struct Pipes {
    reader: tokio::io::DuplexStream,
    writer: ChildStdin,
}
impl AsyncRead for Pipes {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}
impl AsyncWrite for Pipes {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

// 2.4.0's raw client does not apply Config.max_packet_len to reads. Validate
// framing before it sees the length, so its decoder cannot allocate 4 GiB.
async fn bounded_packets<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut input: R,
    mut output: W,
) -> io::Result<()> {
    let mut header = [0; 4];
    let mut buffer = [0; CHUNK];
    loop {
        input.read_exact(&mut header).await?;
        let mut remaining = u32::from_be_bytes(header) as usize;
        if remaining == 0 || remaining > MAX_PACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SFTP packet exceeds 256 KiB",
            ));
        }
        output.write_all(&header).await?;
        while remaining > 0 {
            let count = remaining.min(CHUNK);
            input.read_exact(&mut buffer[..count]).await?;
            output.write_all(&buffer[..count]).await?;
            remaining -= count;
        }
    }
}

fn remote_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 32768 || path.contains('\0') {
        return Err("Remote path must be nonempty, NUL-free, and at most 32768 bytes".into());
    }
    Ok(())
}
fn ssh_program() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join(r"System32\OpenSSH\ssh.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("ssh")
    }
}
fn sftp_error(error: SftpError) -> String {
    format!("SFTP: {error}")
}
fn status(error: &SftpError, code: StatusCode) -> bool {
    matches!(error,SftpError::Status(s) if s.status_code==code)
}

struct Connection {
    session: RawSftpSession,
    child: Child,
    stderr: Arc<Mutex<Vec<u8>>>,
    tasks: Vec<JoinHandle<()>>,
    #[cfg(windows)]
    _job: SshJob,
    #[cfg(unix)]
    process_group: Option<i32>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.session.close_session();
        #[cfg(unix)]
        if let Some(group) = self.process_group {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Connection {
    async fn open(profile: &SshProfile) -> Result<Self, String> {
        let args = build_ssh_args(profile, SshMode::SftpSubsystem)?;
        let mut command = Command::new(ssh_program());
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot start system OpenSSH: {e}"))?;
        #[cfg(windows)]
        let job = SshJob::attach(child.raw_handle())?;
        #[cfg(unix)]
        let process_group = child.id().and_then(|id| i32::try_from(id).ok());
        let stdout = child.stdout.take().ok_or("SSH stdout unavailable")?;
        let stdin = child.stdin.take().ok_or("SSH stdin unavailable")?;
        let mut errors = child.stderr.take().ok_or("SSH stderr unavailable")?;
        let (reader, writer) = tokio::io::duplex(CHUNK * 2);
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let captured = stderr.clone();
        let err_task = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            while let Ok(count) = errors.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                if let Ok(mut bytes) = captured.lock() {
                    let remaining = (64 * 1024usize).saturating_sub(bytes.len());
                    bytes.extend_from_slice(&buffer[..count.min(remaining)]);
                }
            }
        });
        let packet_error = stderr.clone();
        let packet_task = tokio::spawn(async move {
            if let Err(error) = bounded_packets(stdout, writer).await {
                if error.kind() != io::ErrorKind::UnexpectedEof {
                    if let Ok(mut bytes) = packet_error.lock() {
                        let text = format!("\n{error}");
                        let remaining = (64 * 1024usize).saturating_sub(bytes.len());
                        bytes.extend_from_slice(&text.as_bytes()[..text.len().min(remaining)]);
                    }
                }
            }
        });
        let connection = Self {
            session: RawSftpSession::new(Pipes {
                reader,
                writer: stdin,
            }),
            child,
            stderr,
            tasks: vec![err_task, packet_task],
            #[cfg(windows)]
            _job: job,
            #[cfg(unix)]
            process_group,
        };
        match timeout(Duration::from_secs(15), connection.session.init()).await {
            Ok(Ok(version)) if version.version == 3 => Ok(connection),
            result => {
                let error = match result {
                    Ok(Ok(_)) => "Server must support SFTP v3".into(),
                    Ok(Err(e)) => sftp_error(e),
                    Err(_) => "SSH/SFTP initialization timed out".into(),
                };
                connection
                    .finish::<()>(Err(error))
                    .await
                    .map(|_| unreachable!())
            }
        }
    }
    async fn finish<T>(mut self, result: Result<T, String>) -> Result<T, String> {
        let _ = self.session.close_session();
        if timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
        }
        for task in &self.tasks {
            task.abort();
        }
        result.map_err(|error| {
            let detail = self
                .stderr
                .lock()
                .ok()
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes[..bytes.len().min(4096)])
                        .trim()
                        .to_owned()
                })
                .unwrap_or_default();
            if detail.is_empty() {
                error
            } else {
                format!("{error}\nOpenSSH: {detail}")
            }
        })
    }
}

#[cfg(windows)]
struct SshJob {
    _handle: std::os::windows::io::OwnedHandle,
}
#[cfg(windows)]
impl SshJob {
    fn attach(process: Option<std::os::windows::io::RawHandle>) -> Result<Self, String> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        let process = process.ok_or("SSH process handle unavailable")?;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(format!(
                    "Create SSH cleanup job: {}",
                    io::Error::last_os_error()
                ));
            }
            let job = Self {
                _handle: std::os::windows::io::OwnedHandle::from_raw_handle(handle),
            };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(format!(
                    "Configure SSH cleanup job: {}",
                    io::Error::last_os_error()
                ));
            }
            // Tokio exposes the handle after spawning; there is a small unavoidable
            // spawn-to-assignment window before descendants join this job.
            if AssignProcessToJobObject(handle, process) == 0 {
                return Err(format!(
                    "Assign SSH cleanup job: {}",
                    io::Error::last_os_error()
                ));
            }
            Ok(job)
        }
    }
}

pub async fn list_remote(profile: &SshProfile, path: &str) -> Result<RemoteListing, String> {
    remote_path(path)?;
    let connection = Connection::open(profile).await?;
    let result = timeout(
        Duration::from_secs(60),
        list_session(&connection.session, path),
    )
    .await
    .unwrap_or_else(|_| Err("Directory listing timed out".into()));
    connection.finish(result).await
}
async fn list_session(session: &RawSftpSession, path: &str) -> Result<RemoteListing, String> {
    let resolved = session
        .realpath(path)
        .await
        .map_err(sftp_error)?
        .files
        .into_iter()
        .next()
        .ok_or("Server returned no canonical directory")?
        .filename;
    remote_path(&resolved)?;
    let handle = session
        .opendir(resolved.clone())
        .await
        .map_err(sftp_error)?
        .handle;
    let result = async {
        let mut entries = vec![];
        let mut inventory_bytes = resolved.len() * 6 + 64;
        loop {
            let batch = match session.readdir(handle.clone()).await {
                Ok(batch) => batch,
                Err(e) if status(&e, StatusCode::Eof) => break,
                Err(e) => return Err(sftp_error(e)),
            };
            if batch.files.is_empty() {
                return Err("Server returned an empty non-EOF directory batch".into());
            }
            for file in batch.files {
                if file.filename == "." || file.filename == ".." {
                    continue;
                }
                remote_path(&file.filename)?;
                if file.filename.contains('/') {
                    return Err("Server returned an invalid directory entry name".into());
                }
                if entries.len() >= MAX_ENTRIES {
                    return Err("Directory exceeds 20,000 entries".into());
                }
                let full = format!("{}/{}", resolved.trim_end_matches('/'), file.filename);
                remote_path(&full)?;
                let size = file.attrs.size.unwrap_or(0);
                if size > 9_007_199_254_740_991 {
                    return Err("Server file size exceeds the supported integer range".into());
                }
                let entry = RemoteEntry {
                    name: file.filename,
                    path: full,
                    is_dir: file.attrs.file_type().is_dir(),
                    is_symlink: file.attrs.file_type().is_symlink(),
                    size,
                    modified: file.attrs.mtime,
                };
                inventory_bytes += serde_json::to_vec(&entry).map_err(|e| e.to_string())?.len() + 1;
                if inventory_bytes > MAX_INVENTORY {
                    return Err("Remote directory inventory exceeds 16 MiB".into());
                }
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(RemoteListing {
            path: resolved,
            entries,
        })
    }
    .await;
    let closed = session.close(handle).await.map_err(sftp_error);
    result.and_then(|value| closed.map(|_| value))
}

fn ensure_local_absent(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.as_os_str().len() > 32768 {
        return Err(
            "Local destination must be an absolute path of at most 32768 characters".into(),
        );
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err("Local destination already exists; choose a new filename".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
pub async fn download_file(
    profile: &SshProfile,
    remote: &str,
    local: &Path,
) -> Result<TransferResult, String> {
    remote_path(remote)?;
    ensure_local_absent(local)?;
    let connection = Connection::open(profile).await?;
    let result = timeout(
        Duration::from_secs(300),
        download_session(&connection.session, remote, local),
    )
    .await
    .unwrap_or_else(|_| Err("Download exceeded its five-minute deadline".into()));
    connection.finish(result).await
}
async fn download_session(
    session: &RawSftpSession,
    remote: &str,
    local: &Path,
) -> Result<TransferResult, String> {
    let parent = local
        .parent()
        .ok_or("Destination has no parent directory")?;
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut output = tokio::fs::File::from_std(temporary.reopen().map_err(|e| e.to_string())?);
    let handle = session
        .open(remote, OpenFlags::READ, FileAttributes::empty())
        .await
        .map_err(sftp_error)?
        .handle;
    let result = async {
        let attrs = session
            .fstat(handle.clone())
            .await
            .map_err(sftp_error)?
            .attrs;
        if attrs.file_type().is_dir() || attrs.size.is_some_and(|size| size > MAX_TRANSFER) {
            return Err("Download must be a file no larger than 1 GiB".into());
        }
        let expected = attrs.size.ok_or("Server did not report a file size; download cannot be verified")?;
        let mut bytes = 0;
        loop {
            let data = match session.read(handle.clone(), bytes, CHUNK as u32).await {
                Ok(data) => data.data,
                Err(e) if status(&e, StatusCode::Eof) => break,
                Err(e) => return Err(sftp_error(e)),
            };
            if data.is_empty() || data.len() > CHUNK {
                return Err("Server returned an invalid data chunk".into());
            }
            bytes += data.len() as u64;
            if bytes > expected {
                return Err("Remote file grew during download; incomplete output was discarded".into());
            }
            output.write_all(&data).await.map_err(|e| e.to_string())?;
        }
        let after = session.fstat(handle.clone()).await.map_err(sftp_error)?.attrs;
        if bytes != expected || after.size != Some(expected) || attrs.mtime.zip(after.mtime).is_some_and(|(before, after)| before != after) {
            return Err("Remote file changed or ended early during download; incomplete output was discarded".into());
        }
        output.sync_all().await.map_err(|e| e.to_string())?;
        Ok(bytes)
    }
    .await;
    let closed = session.close(handle).await.map_err(sftp_error);
    let bytes = result.and_then(|bytes| closed.map(|_| bytes))?;
    drop(output);
    temporary
        .persist_noclobber(local)
        .map_err(|e| format!("Cannot commit download without overwriting: {}", e.error))?;
    Ok(TransferResult {
        bytes,
        local_path: local.to_string_lossy().into_owned(),
        remote_path: remote.into(),
    })
}

async fn remote_absent(session: &RawSftpSession, path: &str) -> Result<(), String> {
    match session.lstat(path).await {
        Ok(_) => Err("Remote destination already exists; choose a new filename".into()),
        Err(e) if status(&e, StatusCode::NoSuchFile) => Ok(()),
        Err(e) => Err(sftp_error(e)),
    }
}
fn upload_temporary(path: &str) -> Result<String, String> {
    remote_path(path)?;
    let (parent, name) = path.rsplit_once('/').unwrap_or((".", path));
    if name.is_empty() || name == "." || name == ".." {
        return Err("Upload destination must name a file".into());
    }
    let temporary = format!("{parent}/.relay-{}.part", uuid::Uuid::new_v4());
    remote_path(&temporary)?;
    Ok(temporary)
}

#[derive(Clone, Copy)]
enum UploadTemporary {
    NotRequested,
    Unconfirmed,
    Owned,
    CommitRequested,
}

async fn cleanup_upload(
    session: &RawSftpSession,
    temporary: &str,
    state: UploadTemporary,
    remote: &str,
) -> String {
    match state {
        UploadTemporary::NotRequested => String::new(),
        UploadTemporary::Unconfirmed => format!(
            "; temporary file creation was not confirmed; a file may remain at {temporary}. It was not removed because ownership is uncertain"
        ),
        UploadTemporary::Owned | UploadTemporary::CommitRequested => {
            let cleanup = timeout(Duration::from_secs(3), session.remove(temporary)).await;
            let detail = match cleanup {
                Ok(Ok(_)) => String::new(),
                Ok(Err(e)) if status(&e, StatusCode::NoSuchFile) => String::new(),
                _ => format!("; temporary file may remain at {temporary}"),
            };
            if matches!(state, UploadTemporary::CommitRequested) {
                format!("{detail}; upload commit was not acknowledged; {remote} may already contain the completed file. Check that destination before retrying")
            } else { detail }
        }
    }
}

pub async fn upload_file(
    profile: &SshProfile,
    local: &Path,
    remote: &str,
) -> Result<TransferResult, String> {
    remote_path(remote)?;
    if !local.is_absolute() || local.as_os_str().len() > 32768 {
        return Err("Local source must be an absolute path of at most 32768 characters".into());
    }
    let mut input = tokio::fs::File::open(local)
        .await
        .map_err(|e| e.to_string())?;
    let metadata = input.metadata().await.map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_TRANSFER {
        return Err("Upload must be a regular file no larger than 1 GiB".into());
    }
    let temporary = upload_temporary(remote)?;
    let connection = Connection::open(profile).await?;
    let mut temporary_state = UploadTemporary::NotRequested;
    let result = timeout(
        Duration::from_secs(300),
        upload_session(
            &connection.session,
            &mut input,
            remote,
            &temporary,
            &mut temporary_state,
            &metadata,
        ),
    )
    .await
    .unwrap_or_else(|_| Err("Upload exceeded its five-minute deadline".into()));
    let result = match result {
        Ok(bytes) => Ok(TransferResult {
            bytes,
            local_path: local.to_string_lossy().into_owned(),
            remote_path: remote.into(),
        }),
        Err(error) => {
            let detail =
                cleanup_upload(&connection.session, &temporary, temporary_state, remote).await;
            Err(format!("{error}{detail}"))
        }
    };
    connection.finish(result).await
}
async fn upload_session(
    session: &RawSftpSession,
    input: &mut tokio::fs::File,
    remote: &str,
    temporary: &str,
    temporary_state: &mut UploadTemporary,
    initial_metadata: &std::fs::Metadata,
) -> Result<u64, String> {
    remote_absent(session, remote).await?;
    let mut attrs = FileAttributes::empty();
    attrs.permissions = Some(0o600);
    // The server may create the file even if its HANDLE reply never arrives.
    // Only that reply establishes ownership for automatic cleanup.
    *temporary_state = UploadTemporary::Unconfirmed;
    let handle = session
        .open(
            temporary,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            attrs,
        )
        .await
        .map_err(sftp_error)?
        .handle;
    *temporary_state = UploadTemporary::Owned;
    let result = async {
        let mut buffer = vec![0; CHUNK];
        let mut bytes = 0;
        loop {
            let count = input.read(&mut buffer).await.map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if bytes + count as u64 > initial_metadata.len() {
                return Err("Local file grew during upload; temporary output was discarded".into());
            }
            session
                .write(handle.clone(), bytes, buffer[..count].to_vec())
                .await
                .map_err(sftp_error)?;
            bytes += count as u64;
        }
        let after = input.metadata().await.map_err(|e| e.to_string())?;
        if bytes != initial_metadata.len() || after.len() != initial_metadata.len() || after.modified().ok() != initial_metadata.modified().ok() {
            return Err("Local file changed or ended early during upload; temporary output was discarded".into());
        }
        let uploaded = session.fstat(handle.clone()).await.map_err(sftp_error)?.attrs;
        if uploaded.size != Some(bytes) {
            return Err("Server file size does not match the uploaded bytes; temporary output was discarded".into());
        }
        Ok(bytes)
    }
    .await;
    let closed = session.close(handle).await.map_err(sftp_error);
    let bytes = result.and_then(|bytes| closed.map(|_| bytes))?;
    remote_absent(session, remote).await?;
    // SSH_FXP_RENAME v3 fails if the destination exists. Never use the
    // posix-rename extension, whose replacement semantics would clobber it.
    *temporary_state = UploadTemporary::CommitRequested;
    if let Err(error) = session.rename(temporary, remote).await {
        if matches!(&error, SftpError::Status(_)) {
            *temporary_state = UploadTemporary::Owned;
        }
        return Err(sftp_error(error));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_are_binary_not_shell_commands() {
        assert!(remote_path("/line\nbreak ' ; $(command)").is_ok());
        assert!(remote_path("/bad\0path").is_err());
        assert!(upload_temporary("/").is_err());
        assert!(upload_temporary("/folder/file")
            .unwrap()
            .starts_with("/folder/.relay-"));
    }
    #[test]
    fn refuses_existing_local_destination() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(ensure_local_absent(file.path()).is_err());
    }
    #[test]
    fn unconfirmed_upload_creation_is_reported_without_remote_deletion() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (client, mut server) = tokio::io::duplex(1024);
            let session = RawSftpSession::new(client);
            let path = "/upload/.relay-unconfirmed.part";
            let detail = cleanup_upload(
                &session,
                path,
                UploadTemporary::Unconfirmed,
                "/upload/result",
            )
            .await;
            assert!(detail.contains(path));
            assert!(detail.contains("ownership is uncertain"));
            assert!(cleanup_upload(
                &session,
                path,
                UploadTemporary::NotRequested,
                "/upload/result"
            )
            .await
            .is_empty());
            // No INIT or REMOVE is sent for an unconfirmed file, even on a live transport.
            assert!(timeout(Duration::from_millis(20), server.read_u8())
                .await
                .is_err());
        });
    }
    #[test]
    fn inbound_length_is_checked_before_forwarding() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let bytes = ((MAX_PACKET + 1) as u32).to_be_bytes();
            let mut output = Vec::new();
            let result = bounded_packets(&bytes[..], &mut output).await;
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert!(output.is_empty());
            let bytes = [0, 0, 0, 1, 42];
            let mut output = Vec::new();
            let result = bounded_packets(&bytes[..], &mut output).await;
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
            assert_eq!(output, bytes);
        });
    }
}
