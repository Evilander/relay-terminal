use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

const MAX_SESSIONS: usize = 32;
const MAX_INPUT: usize = 64 * 1024;
const OUTPUT_WINDOW: usize = 256 * 1024;
const READ_CHUNK: usize = 16 * 1024;
const MAX_SEQUENCE: u64 = (1_u64 << 53) - 1;

type EventSink = Arc<dyn Fn(TerminalEvent) -> bool + Send + Sync>;

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    Data { id: String, seq: u64, data: Vec<u8> },
    Exit { id: String, code: Option<u32> },
    Error { id: String, message: String },
}

enum Slot {
    Starting(Arc<AtomicBool>),
    Active(Arc<Session>),
}

#[derive(Default)]
struct Registry {
    sessions: Mutex<HashMap<String, Slot>>,
}

#[derive(Default)]
pub struct TerminalManager {
    registry: Arc<Registry>,
}

#[derive(Default)]
struct Flow {
    stopped: bool,
    last_sequence: u64,
    acknowledged: u64,
    pending: VecDeque<(u64, usize)>,
    bytes: usize,
    reader_done: bool,
    waiter_done: bool,
    exit_sent: bool,
    code: Option<u32>,
}

struct Session {
    id: String,
    registry: Weak<Registry>,
    sink: EventSink,
    flow: Mutex<Flow>,
    credit: Condvar,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    #[cfg(unix)]
    process_group: Option<i32>,
    #[cfg(windows)]
    job: Mutex<Option<WindowsJob>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn size(cols: u16, rows: u16) -> Result<PtySize, String> {
    if cols == 0
        || rows == 0
        || cols > 5_000
        || rows > 5_000
        || u32::from(cols) * u32::from(rows) > 1_000_000
    {
        return Err(
            "Terminal grid must contain 1–1,000,000 cells, with each dimension at most 5000".into(),
        );
    }
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn validate(id: &str, spec: &SpawnSpec) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err("Invalid terminal session ID".into());
    }
    if spec.program.is_empty() || spec.program.len() > 32_768 || spec.program.contains('\0') {
        return Err("Invalid terminal program".into());
    }
    if spec.args.len() > 256
        || spec.args.iter().any(|arg| arg.contains('\0'))
        || spec
            .args
            .iter()
            .try_fold(0_usize, |total, arg| total.checked_add(arg.len()))
            .is_none_or(|total| total > 1024 * 1024)
    {
        return Err("Terminal arguments exceed the supported bounds or contain NUL".into());
    }
    if let Some(cwd) = &spec.cwd {
        if !cwd.is_dir() {
            return Err("Terminal working directory does not exist".into());
        }
    }
    Ok(())
}

impl TerminalManager {
    pub fn spawn(
        &self,
        id: String,
        spec: SpawnSpec,
        cols: u16,
        rows: u16,
        sink: EventSink,
    ) -> Result<Option<u32>, String> {
        validate(&id, &spec)?;
        let dimensions = size(cols, rows)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut sessions = lock(&self.registry.sessions);
            if sessions.contains_key(&id) {
                return Err("Terminal session ID is already open".into());
            }
            if sessions.len() >= MAX_SESSIONS {
                return Err("At most 32 terminals can be open".into());
            }
            sessions.insert(id.clone(), Slot::Starting(cancelled.clone()));
        }
        let result = self.spawn_reserved(&id, spec, dimensions, sink, &cancelled);
        if result.is_err() {
            let mut sessions = lock(&self.registry.sessions);
            if matches!(sessions.get(&id), Some(Slot::Starting(token)) if Arc::ptr_eq(token, &cancelled))
            {
                sessions.remove(&id);
            }
        }
        result
    }

    fn spawn_reserved(
        &self,
        id: &str,
        spec: SpawnSpec,
        dimensions: PtySize,
        sink: EventSink,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Option<u32>, String> {
        let pair = native_pty_system()
            .openpty(dimensions)
            .map_err(|e| format!("Open PTY: {e}"))?;
        // Obtain the pipe endpoints before starting a process so setup failures cannot orphan it.
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Open PTY reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Open PTY writer: {e}"))?;
        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        if let Some(cwd) = spec.cwd {
            command.cwd(cwd);
        }
        command.env("TERM", "xterm-256color");
        #[cfg(windows)]
        let job = WindowsJob::new()?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| format!("Start terminal: {e}"))?;
        drop(pair.slave);
        #[cfg(windows)]
        if let Err(error) = job.assign(child.as_raw_handle()) {
            let _ = child.kill();
            // Do not wait with unread ConPTY output: dropping the reader unblocks its teardown.
            drop(reader);
            drop(writer);
            drop(pair.master);
            let _ = child.wait();
            return Err(error);
        }
        let pid = child.process_id();
        let session = Arc::new(Session {
            id: id.to_owned(),
            registry: Arc::downgrade(&self.registry),
            sink,
            flow: Mutex::new(Flow::default()),
            credit: Condvar::new(),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(child.clone_killer()),
            #[cfg(unix)]
            process_group: pid.and_then(|pid| i32::try_from(pid).ok()),
            #[cfg(windows)]
            job: Mutex::new(Some(job)),
        });
        let installed = {
            let mut sessions = lock(&self.registry.sessions);
            if !cancelled.load(Ordering::Acquire)
                && matches!(sessions.get(id), Some(Slot::Starting(token)) if Arc::ptr_eq(token, cancelled))
            {
                sessions.insert(id.to_owned(), Slot::Active(session.clone()));
                true
            } else {
                false
            }
        };
        if !installed {
            session.stop();
            drop(reader);
            session.release_pty();
            let _ = child.wait();
            return Err("Terminal opening was cancelled".into());
        }

        let read_session = session.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("terminal-reader".into())
            .spawn(move || read_session.read_output(reader))
        {
            session.stop();
            session.release_pty();
            let _ = child.wait();
            session.finish(true, None);
            session.finish(false, None);
            return Err(format!("Start terminal reader: {error}"));
        }
        let wait_session = session.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("terminal-waiter".into())
            .spawn(move || {
                let outcome = child.wait();
                let code = match outcome {
                    Ok(status) => Some(status.exit_code()),
                    Err(error) => {
                        wait_session.error(format!("Wait for terminal: {error}"));
                        wait_session.stop();
                        None
                    }
                };
                // Closing ConPTY produces EOF and can block while the reader drains. This
                // runs independently of that reader and holds no registry or flow lock.
                wait_session.end_descendants();
                wait_session.release_pty();
                wait_session.finish(false, code);
            })
        {
            session.stop();
            session.release_pty();
            session.finish(false, None);
            return Err(format!("Start terminal waiter: {error}"));
        }
        Ok(pid)
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, String> {
        match lock(&self.registry.sessions).get(id) {
            Some(Slot::Active(session)) => Ok(session.clone()),
            Some(Slot::Starting(_)) => Err("Terminal is still opening".into()),
            None => Err("Terminal is not open".into()),
        }
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        if data.len() > MAX_INPUT {
            return Err("Terminal input exceeds 64 KiB per write".into());
        }
        let session = self.session(id)?;
        if lock(&session.flow).stopped {
            return Err("Terminal is closing".into());
        }
        let mut writer = lock(&session.writer);
        let writer = writer.as_mut().ok_or("Terminal input is closed")?;
        writer
            .write_all(data)
            .and_then(|_| writer.flush())
            .map_err(|e| format!("Write terminal: {e}"))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let dimensions = size(cols, rows)?;
        let session = self.session(id)?;
        let master = lock(&session.master);
        master
            .as_ref()
            .ok_or("Terminal is closed")?
            .resize(dimensions)
            .map_err(|e| format!("Resize terminal: {e}"))
    }

    /// Acknowledge all delivered chunks up to and including `seq`. Repeated ACKs are harmless.
    pub fn ack(&self, id: &str, seq: u64) -> Result<(), String> {
        let session = match self.session(id) {
            Ok(session) => session,
            Err(_) => return Ok(()),
        };
        let mut flow = lock(&session.flow);
        if seq > flow.last_sequence {
            return Err("Acknowledgement refers to unsent terminal output".into());
        }
        if seq <= flow.acknowledged {
            return Ok(());
        }
        while flow
            .pending
            .front()
            .is_some_and(|(number, _)| *number <= seq)
        {
            if let Some((_, bytes)) = flow.pending.pop_front() {
                flow.bytes -= bytes;
            }
        }
        flow.acknowledged = seq;
        session.credit.notify_all();
        Ok(())
    }

    pub fn close(&self, id: &str) -> Result<(), String> {
        let session = {
            let mut sessions = lock(&self.registry.sessions);
            match sessions.get(id) {
                Some(Slot::Starting(token)) => {
                    token.store(true, Ordering::Release);
                    sessions.remove(id);
                    None
                }
                Some(Slot::Active(session)) => Some(session.clone()),
                None => None,
            }
        };
        if let Some(session) = session {
            session.stop();
        }
        Ok(())
    }

    pub fn close_all(&self) {
        let sessions: Vec<_> = {
            let mut sessions = lock(&self.registry.sessions);
            sessions.drain().map(|(_, slot)| slot).collect()
        };
        for slot in sessions {
            match slot {
                Slot::Starting(token) => token.store(true, Ordering::Release),
                Slot::Active(session) => session.stop(),
            }
        }
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl Session {
    fn send(&self, event: TerminalEvent) -> bool {
        let delivered =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.sink)(event)))
                .unwrap_or(false);
        if !delivered {
            self.stop();
        }
        delivered
    }

    fn error(&self, message: String) {
        self.send(TerminalEvent::Error {
            id: self.id.clone(),
            message,
        });
    }

    fn stop(&self) {
        {
            let mut flow = lock(&self.flow);
            if flow.stopped {
                return;
            }
            flow.stopped = true;
            flow.pending.clear();
            flow.bytes = 0;
            self.credit.notify_all();
        }
        self.end_descendants();
        // The independent killer never contends with a blocking Child::wait().
        let _ = lock(&self.killer).kill();
    }

    fn end_descendants(&self) {
        #[cfg(windows)]
        {
            let job = lock(&self.job).take();
            drop(job);
        }
        #[cfg(unix)]
        {
            let foreground = lock(&self.master)
                .as_ref()
                .and_then(|master| master.process_group_leader());
            for group in [foreground, self.process_group].into_iter().flatten() {
                // The child calls setsid in portable-pty. Shell jobs can use a different
                // foreground group; terminate both that group and the initial shell group.
                if group > 0 {
                    unsafe {
                        libc::killpg(group, libc::SIGKILL);
                    }
                }
            }
        }
    }

    fn release_pty(&self) {
        let master = lock(&self.master).take();
        drop(master);
        let writer = lock(&self.writer).take();
        drop(writer);
    }

    fn read_output(self: Arc<Self>, mut reader: Box<dyn Read + Send>) {
        let mut buffer = vec![0_u8; READ_CHUNK];
        loop {
            if lock(&self.flow).stopped {
                break;
            }
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                // Linux PTYs return EIO when their last slave closes.
                #[cfg(target_os = "linux")]
                Err(error) if error.raw_os_error() == Some(5) => break,
                Err(error) => {
                    if !lock(&self.flow).stopped {
                        self.error(format!("Read terminal: {error}"));
                    }
                    self.stop();
                    break;
                }
            };
            let sequence = {
                let mut flow = lock(&self.flow);
                while !flow.stopped && flow.bytes + count > OUTPUT_WINDOW {
                    flow = self
                        .credit
                        .wait(flow)
                        .unwrap_or_else(|error| error.into_inner());
                }
                if flow.stopped {
                    break;
                }
                if flow.last_sequence == MAX_SEQUENCE {
                    drop(flow);
                    self.error("Terminal output sequence limit reached".into());
                    self.stop();
                    break;
                }
                flow.last_sequence += 1;
                let sequence = flow.last_sequence;
                flow.pending.push_back((sequence, count));
                flow.bytes += count;
                sequence
            };
            if !self.send(TerminalEvent::Data {
                id: self.id.clone(),
                seq: sequence,
                data: buffer[..count].to_vec(),
            }) {
                break;
            }
        }
        drop(reader);
        self.finish(true, None);
    }

    fn finish(self: &Arc<Self>, reader: bool, code: Option<u32>) {
        let event = {
            let mut flow = lock(&self.flow);
            if reader {
                flow.reader_done = true;
            } else {
                flow.waiter_done = true;
                flow.code = code;
            }
            if !flow.reader_done || !flow.waiter_done || flow.exit_sent {
                return;
            }
            flow.exit_sent = true;
            Some(TerminalEvent::Exit {
                id: self.id.clone(),
                code: flow.code,
            })
        };
        if let Some(registry) = self.registry.upgrade() {
            let mut sessions = lock(&registry.sessions);
            if matches!(sessions.get(&self.id), Some(Slot::Active(session)) if Arc::ptr_eq(session, self))
            {
                sessions.remove(&self.id);
            }
        }
        if let Some(event) = event {
            self.send(event);
        }
    }
}

#[cfg(windows)]
struct WindowsJob(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl WindowsJob {
    fn new() -> Result<Self, String> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(format!(
                    "Create terminal job: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let job = Self(std::os::windows::io::OwnedHandle::from_raw_handle(handle));
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
                    "Configure terminal job: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(job)
        }
    }

    fn assign(&self, child: Option<std::os::windows::io::RawHandle>) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        let child = child.ok_or("Terminal child has no Windows process handle")?;
        // portable-pty starts the child before exposing its handle. Assignment is immediate,
        // but it cannot cover a descendant launched in the preceding spawn-to-assign interval.
        if unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0.as_raw_handle(),
                child,
            )
        } == 0
        {
            return Err(format!(
                "Assign terminal process to cleanup job: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    fn command(windows: &str, unix: &str) -> SpawnSpec {
        #[cfg(windows)]
        {
            let _ = unix;
            let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            SpawnSpec {
                program: PathBuf::from(root)
                    .join("System32/WindowsPowerShell/v1.0/powershell.exe")
                    .to_string_lossy()
                    .into_owned(),
                args: vec![
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    windows.into(),
                ],
                cwd: None,
            }
        }
        #[cfg(unix)]
        {
            let _ = windows;
            SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), unix.into()],
                cwd: None,
            }
        }
    }

    fn channel() -> (EventSink, Receiver<TerminalEvent>) {
        let (sender, receiver) = mpsc::channel();
        (Arc::new(move |event| sender.send(event).is_ok()), receiver)
    }

    fn initial_cursor(manager: &TerminalManager, id: &str) {
        #[cfg(windows)]
        manager.write(id, b"\x1b[1;1R").unwrap();
        #[cfg(not(windows))]
        let _ = (manager, id);
    }

    fn receive(receiver: &Receiver<TerminalEvent>, deadline: Instant) -> TerminalEvent {
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("terminal event deadline exceeded")
    }

    fn drain(
        manager: &TerminalManager,
        receiver: &Receiver<TerminalEvent>,
        id: &str,
    ) -> (Vec<u8>, Option<u32>) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut bytes = Vec::new();
        loop {
            match receive(receiver, deadline) {
                TerminalEvent::Data { seq, data, .. } => {
                    bytes.extend_from_slice(&data);
                    manager.ack(id, seq).unwrap();
                }
                TerminalEvent::Error { message, .. } => panic!("{message}"),
                TerminalEvent::Exit { code, .. } => return (bytes, code),
            }
        }
    }

    #[test]
    fn validation_and_reservations_are_bounded() {
        assert!(size(0, 80).is_err());
        assert!(size(80, 0).is_err());
        assert!(size(5_001, 24).is_err());
        assert!(size(5_000, 5_000).is_err());
        assert!(size(120, 40).is_ok());
        let mut spec = command("exit 0", "exit 0");
        assert!(validate("", &spec).is_err());
        spec.args = vec!["\0".into()];
        assert!(validate("test", &spec).is_err());
        let manager = TerminalManager::default();
        for index in 0..MAX_SESSIONS {
            lock(&manager.registry.sessions).insert(
                index.to_string(),
                Slot::Starting(Arc::new(AtomicBool::new(false))),
            );
        }
        let (sink, _) = channel();
        assert!(manager
            .spawn("33".into(), command("exit 0", "exit 0"), 80, 24, sink)
            .unwrap_err()
            .contains("32"));
        assert!(manager
            .write("0", &vec![0; MAX_INPUT + 1])
            .unwrap_err()
            .contains("64 KiB"));
        manager.close_all();
        assert!(lock(&manager.registry.sessions).is_empty());
    }

    #[test]
    fn pty_round_trips_input_unicode_resize_and_final_output() {
        let manager = TerminalManager::default();
        let (sink, receiver) = channel();
        let spec = command(
            "[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); [Console]::WriteLine('READY'); $line = [Console]::ReadLine(); [Console]::WriteLine('ECHO:' + $line); [Console]::Write('λ雪FINAL'); exit 7",
            "printf 'READY\\n'; IFS= read -r line; printf 'ECHO:%s\\nλ雪FINAL' \"$line\"; exit 7",
        );
        assert!(manager
            .spawn("roundtrip".into(), spec, 80, 24, sink)
            .unwrap()
            .is_some());
        initial_cursor(&manager, "roundtrip");
        manager.resize("roundtrip", 121, 37).unwrap();
        let session = manager.session("roundtrip").unwrap();
        let dimensions = lock(&session.master).as_ref().unwrap().get_size().unwrap();
        assert_eq!((dimensions.cols, dimensions.rows), (121, 37));
        assert!(manager.ack("roundtrip", u64::MAX).is_err());
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("READY") {
            match receive(&receiver, deadline) {
                TerminalEvent::Data { seq, data, .. } => {
                    output.extend_from_slice(&data);
                    manager.ack("roundtrip", seq).unwrap();
                }
                event => panic!("unexpected event: {event:?}"),
            }
        }
        #[cfg(windows)]
        manager.write("roundtrip", b"hello-relay\r").unwrap();
        #[cfg(unix)]
        manager.write("roundtrip", b"hello-relay\n").unwrap();
        let (remaining, code) = drain(&manager, &receiver, "roundtrip");
        output.extend(remaining);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("ECHO:hello-relay"), "{output:?}");
        assert!(output.contains("λ雪FINAL"), "{output:?}");
        assert_eq!(code, Some(7));
        assert!(manager.session("roundtrip").is_err());
        assert!(receiver.try_recv().is_err());
    }

    fn flood() -> SpawnSpec {
        command(
            "[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); for ($i = 0; $i -lt 16384; $i++) { [Console]::WriteLine(('ROW{0:D6}:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' -f $i)) }; [Console]::Write('TAIL'); exit 0",
            "i=0; while [ \"$i\" -lt 16384 ]; do printf 'ROW%06d:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' \"$i\"; i=$((i+1)); done; printf TAIL; exit 0",
        )
    }

    fn wait_for_backpressure(manager: &TerminalManager, id: &str) -> Arc<Session> {
        let session = manager.session(id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let bytes = lock(&session.flow).bytes;
            assert!(bytes <= OUTPUT_WINDOW);
            if bytes > OUTPUT_WINDOW - READ_CHUNK {
                return session;
            }
            assert!(Instant::now() < deadline, "PTY never reached output window");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn backpressure_bounds_output_and_ack_resumes_without_losing_tail() {
        let manager = TerminalManager::default();
        let (sink, receiver) = channel();
        manager.spawn("flow".into(), flood(), 80, 24, sink).unwrap();
        initial_cursor(&manager, "flow");
        let session = wait_for_backpressure(&manager, "flow");
        std::thread::sleep(Duration::from_millis(100));
        assert!(lock(&session.flow).bytes <= OUTPUT_WINDOW);
        assert!(!lock(&session.flow).exit_sent);
        let (bytes, code) = drain(&manager, &receiver, "flow");
        // ConPTY may redraw cells, so its byte stream is not an exact copy of stdout.
        // Unique short records detect omissions while allowing those redraws.
        let records: std::collections::HashSet<_> = bytes
            .windows(10)
            .filter(|record| {
                record.starts_with(b"ROW")
                    && record[9] == b':'
                    && record[3..9].iter().all(u8::is_ascii_digit)
            })
            .map(|record| {
                std::str::from_utf8(&record[3..9])
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
            })
            .collect();
        assert_eq!(records.len(), 16_384);
        assert!((0..16_384).all(|number| records.contains(&number)));
        assert!(String::from_utf8_lossy(&bytes).contains("TAIL"));
        assert_eq!(code, Some(0));
    }

    #[test]
    fn close_unblocks_unacknowledged_output() {
        let manager = TerminalManager::default();
        let (sink, receiver) = channel();
        manager
            .spawn("close-flow".into(), flood(), 80, 24, sink)
            .unwrap();
        initial_cursor(&manager, "close-flow");
        wait_for_backpressure(&manager, "close-flow");
        manager.close("close-flow").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if matches!(receive(&receiver, deadline), TerminalEvent::Exit { .. }) {
                break;
            }
        }
        assert!(manager.session("close-flow").is_err());
        manager.close("close-flow").unwrap();
    }

    #[test]
    fn detached_sink_terminates_and_releases_session() {
        let manager = TerminalManager::default();
        manager
            .spawn(
                "detached".into(),
                command(
                    "[Console]::Write('started'); Start-Sleep -Seconds 60",
                    "printf started; sleep 60",
                ),
                80,
                24,
                Arc::new(|_| false),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while manager.session("detached").is_ok() {
            assert!(Instant::now() < deadline, "detached terminal did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn synchronous_ack_does_not_deadlock_and_is_idempotent() {
        let manager = Arc::new(TerminalManager::default());
        let weak = Arc::downgrade(&manager);
        let (sender, receiver) = mpsc::channel();
        let sink = Arc::new(move |event: TerminalEvent| {
            if let TerminalEvent::Data { id, seq, .. } = &event {
                let manager = weak.upgrade().unwrap();
                manager.ack(id, *seq).unwrap();
                manager.ack(id, *seq).unwrap();
            }
            sender.send(event).is_ok()
        });
        manager
            .spawn(
                "sync-ack".into(),
                command("[Console]::Write('done'); exit 0", "printf done; exit 0"),
                80,
                24,
                sink,
            )
            .unwrap();
        initial_cursor(&manager, "sync-ack");
        assert_eq!(drain(&manager, &receiver, "sync-ack").1, Some(0));
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_terminates_spawned_descendant() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        let manager = TerminalManager::default();
        let (sink, receiver) = channel();
        let spec = command(
            "$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 60\"' -WindowStyle Hidden -PassThru; [Console]::WriteLine('CHILD:' + $child.Id + ':END'); Start-Sleep -Seconds 60",
            "",
        );
        manager
            .spawn("descendant".into(), spec, 120, 30, sink)
            .unwrap();
        initial_cursor(&manager, "descendant");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut bytes = Vec::new();
        let child_pid = loop {
            match receive(&receiver, deadline) {
                TerminalEvent::Data { seq, data, .. } => {
                    bytes.extend(data);
                    manager.ack("descendant", seq).unwrap();
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some(rest) = text.split("CHILD:").nth(1) {
                        if let Some(pid) =
                            rest.split(":END").next().filter(|_| rest.contains(":END"))
                        {
                            break pid.parse::<u32>().expect("child PID");
                        }
                    }
                }
                event => panic!("unexpected event: {event:?}"),
            }
        };
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child_pid) };
        assert!(!handle.is_null(), "could not observe test descendant");
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        manager.close("descendant").unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 5_000) },
            WAIT_OBJECT_0
        );
        drain(&manager, &receiver, "descendant");
    }
}
