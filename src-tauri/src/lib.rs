pub mod bookmarks;
pub mod connection_files;
pub mod connections;
pub mod sftp;
pub mod terminal;

use connections::{Settings, SettingsStore, SshMode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{ipc::Channel, Manager, State};
use terminal::{SpawnSpec, TerminalEvent, TerminalManager};

struct AppState {
    terminals: Arc<TerminalManager>,
    frontend_generation: Arc<AtomicU64>,
    settings: Arc<SettingsStore>,
    _settings_lock: std::fs::File,
}

impl AppState {
    fn detach_frontend(&self) {
        // Invalidate queued opens and old Channel callbacks before closing active PTYs.
        self.frontend_generation.fetch_add(1, Ordering::AcqRel);
        self.terminals.close_all();
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellInfo {
    id: String,
    name: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: &'static str,
    platform: &'static str,
    home: String,
    shells: Vec<ShellInfo>,
    ssh_available: bool,
    sftp_available: bool,
}

fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|path| path.is_absolute())
        .map(|path| path.join(name))
        .find(|path| path.is_file())
}

fn user_home() -> PathBuf {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn shells() -> Vec<ShellInfo> {
    let mut shells = Vec::new();
    #[cfg(windows)]
    {
        if let Some(path) = executable("pwsh.exe") {
            shells.push(ShellInfo {
                id: "pwsh".into(),
                name: "PowerShell 7".into(),
                path: path.to_string_lossy().into(),
            });
        }
        let windows = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        for (id, name, suffix) in [
            (
                "powershell",
                "Windows PowerShell",
                r"System32\WindowsPowerShell\v1.0\powershell.exe",
            ),
            ("cmd", "Command Prompt", r"System32\cmd.exe"),
        ] {
            let path = windows.join(suffix);
            if path.is_file() {
                shells.push(ShellInfo {
                    id: id.into(),
                    name: name.into(),
                    path: path.to_string_lossy().into(),
                });
            }
        }
        if let Some(path) = executable("wsl.exe") {
            shells.push(ShellInfo {
                id: "wsl".into(),
                name: "WSL".into(),
                path: path.to_string_lossy().into(),
            });
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(path) = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_file())
        {
            shells.push(ShellInfo {
                id: "default".into(),
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                path: path.to_string_lossy().into(),
            });
        }
        for name in ["zsh", "bash", "sh"] {
            if let Some(path) = executable(name) {
                let text = path.to_string_lossy().into_owned();
                if !shells.iter().any(|shell| shell.path == text) {
                    shells.push(ShellInfo {
                        id: name.into(),
                        name: name.into(),
                        path: text,
                    });
                }
            }
        }
    }
    shells
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        home: user_home().to_string_lossy().into(),
        shells: shells(),
        ssh_available: executable(if cfg!(windows) { "ssh.exe" } else { "ssh" }).is_some(),
        sftp_available: sftp_available(),
    }
}

fn sftp_available() -> bool {
    // The file browser speaks the SFTP protocol over ssh; it does not invoke sftp.
    #[cfg(windows)]
    {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join(r"System32\OpenSSH\ssh.exe")
            .is_file()
    }
    #[cfg(not(windows))]
    {
        executable("ssh").is_some()
    }
}

#[tauri::command]
async fn load_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    let settings = state.settings.clone();
    tauri::async_runtime::spawn_blocking(move || settings.load())
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn save_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    let store = state.settings.clone();
    tauri::async_runtime::spawn_blocking(move || store.save(&settings))
        .await
        .map_err(|error| error.to_string())?
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SessionKind {
    Local,
    Ssh,
    Sftp,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenRequest {
    id: String,
    kind: SessionKind,
    shell: Option<String>,
    cwd: Option<String>,
    profile_id: Option<String>,
    cols: u16,
    rows: u16,
}

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    pid: Option<u32>,
    title: String,
}

fn spawn_spec(
    request: &OpenRequest,
    settings: &SettingsStore,
) -> Result<(SpawnSpec, String), String> {
    uuid::Uuid::parse_str(&request.id).map_err(|_| "Invalid session identifier".to_string())?;
    match request.kind {
        SessionKind::Local => {
            let available = shells();
            let shell = match &request.shell {
                Some(id) => available.into_iter().find(|shell| &shell.id == id),
                None => available.into_iter().next(),
            }
            .ok_or_else(|| "The selected shell is not installed".to_string())?;
            let cwd = request
                .cwd
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(user_home);
            if !cwd.is_absolute() || !cwd.is_dir() {
                return Err("The working directory must be an existing absolute path".into());
            }
            let args = match shell.id.as_str() {
                "pwsh" | "powershell" => vec!["-NoLogo".into()],
                "cmd" => vec!["/D".into()],
                "wsl" => vec![],
                _ => vec!["-l".into()],
            };
            Ok((
                SpawnSpec {
                    program: shell.path,
                    args,
                    cwd: Some(cwd),
                },
                shell.name,
            ))
        }
        SessionKind::Ssh | SessionKind::Sftp => {
            let profile = settings.profile(
                request
                    .profile_id
                    .as_deref()
                    .ok_or("Select a saved connection")?,
            )?;
            let (name, mode) = if matches!(request.kind, SessionKind::Sftp) {
                (
                    if cfg!(windows) { "sftp.exe" } else { "sftp" },
                    SshMode::SftpTerminal,
                )
            } else {
                (
                    if cfg!(windows) { "ssh.exe" } else { "ssh" },
                    SshMode::Terminal,
                )
            };
            let program = executable(name)
                .ok_or_else(|| format!("{name} is not installed or not on PATH"))?;
            let args = connections::build_ssh_args(&profile, mode)?;
            Ok((
                SpawnSpec {
                    program: program.to_string_lossy().into(),
                    args,
                    cwd: Some(user_home()),
                },
                profile.name,
            ))
        }
    }
}

#[tauri::command]
async fn open_session(
    state: State<'_, AppState>,
    request: OpenRequest,
    on_event: Channel<TerminalEvent>,
) -> Result<SessionInfo, String> {
    let generation = state.frontend_generation.clone();
    let opened_generation = generation.load(Ordering::Acquire);
    let settings = state.settings.clone();
    let terminals = state.terminals.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if generation.load(Ordering::Acquire) != opened_generation {
            return Err("The terminal frontend was reloaded or closed".into());
        }
        let (spec, title) = spawn_spec(&request, &settings)?;
        let id = request.id;
        let sink_generation = generation.clone();
        let sink = Arc::new(move |event| {
            sink_generation.load(Ordering::Acquire) == opened_generation
                && on_event.send(event).is_ok()
        });
        let pid = terminals.spawn(id.clone(), spec, request.cols, request.rows, sink)?;
        if generation.load(Ordering::Acquire) != opened_generation {
            let _ = terminals.close(&id);
            return Err("The terminal frontend was reloaded or closed".into());
        }
        Ok(SessionInfo { id, pid, title })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn write_session(state: State<'_, AppState>, id: String, data: String) -> Result<(), String> {
    write_session_bytes(state, id, data.into_bytes()).await
}

#[tauri::command]
async fn write_session_bytes(
    state: State<'_, AppState>,
    id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    if data.len() > 65_536 {
        return Err("Input chunk exceeds 64 KiB".into());
    }
    let terminals = state.terminals.clone();
    tauri::async_runtime::spawn_blocking(move || terminals.write(&id, &data))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn resize_session(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let terminals = state.terminals.clone();
    tauri::async_runtime::spawn_blocking(move || terminals.resize(&id, cols, rows))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn ack_session(state: State<'_, AppState>, id: String, seq: u64) -> Result<(), String> {
    state.terminals.ack(&id, seq)
}

#[tauri::command]
async fn close_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let terminals = state.terminals.clone();
    tauri::async_runtime::spawn_blocking(move || terminals.close(&id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn list_remote(
    state: State<'_, AppState>,
    profile_id: String,
    path: String,
) -> Result<sftp::RemoteListing, String> {
    let profile = saved_profile(state.settings.clone(), profile_id).await?;
    sftp::list_remote(&profile, &path).await
}

#[tauri::command]
async fn download_file(
    state: State<'_, AppState>,
    profile_id: String,
    remote_path: String,
    local_path: String,
) -> Result<sftp::TransferResult, String> {
    let profile = saved_profile(state.settings.clone(), profile_id).await?;
    sftp::download_file(&profile, &remote_path, Path::new(&local_path)).await
}

#[tauri::command]
async fn upload_file(
    state: State<'_, AppState>,
    profile_id: String,
    local_path: String,
    remote_path: String,
) -> Result<sftp::TransferResult, String> {
    let profile = saved_profile(state.settings.clone(), profile_id).await?;
    sftp::upload_file(&profile, Path::new(&local_path), &remote_path).await
}

#[tauri::command]
async fn inspect_bookmarks(path: String) -> Result<bookmarks::BookmarkInventory, String> {
    tauri::async_runtime::spawn_blocking(move || bookmarks::inspect_bookmarks(Path::new(&path)))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn inspect_connection_file(path: String) -> Result<connection_files::ConnectionFile, String> {
    tauri::async_runtime::spawn_blocking(move || {
        connection_files::inspect_connection_file(Path::new(&path))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn export_connections(state: State<'_, AppState>, path: String) -> Result<usize, String> {
    let settings = state.settings.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let profiles = settings.load()?.profiles;
        connection_files::export_connection_file(Path::new(&path), &profiles)
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn saved_profile(
    settings: Arc<SettingsStore>,
    id: String,
) -> Result<connections::SshProfile, String> {
    tauri::async_runtime::spawn_blocking(move || settings.profile(&id))
        .await
        .map_err(|error| error.to_string())?
}

pub fn run() {
    let context = tauri::generate_context!();
    #[cfg(debug_assertions)]
    let context = {
        let mut context = context;
        if let Ok(port) = std::env::var("RELAY_DEBUG_PORT") {
            let port: std::num::NonZeroU16 =
                port.parse().expect("RELAY_DEBUG_PORT must be 1–65535");
            let window = context
                .config_mut()
                .app
                .windows
                .iter_mut()
                .find(|window| window.label == "main")
                .expect("Missing main window");
            // Elevated WebView2 hosts ignore debugging environment overrides.
            window.additional_browser_args = Some(format!(
                "--remote-debugging-address=127.0.0.1 --remote-debugging-port={port}"
            ));
        }
        context
    };
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config = match std::env::var_os("RELAY_CONFIG_DIR") {
                Some(path) => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() {
                        return Err("RELAY_CONFIG_DIR must be absolute".into());
                    }
                    path
                }
                None => app.path().app_config_dir()?,
            };
            std::fs::create_dir_all(&config)?;
            let settings_lock = connections::lock_settings_directory(&config)?;
            app.manage(AppState {
                terminals: Arc::new(TerminalManager::default()),
                frontend_generation: Arc::new(AtomicU64::new(0)),
                settings: Arc::new(SettingsStore::new(config.join("settings.json"))),
                _settings_lock: settings_lock,
            });
            Ok(())
        })
        .on_page_load(|webview, payload| {
            if webview.label() == "main"
                && matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
            {
                if let Some(state) = webview.try_state::<AppState>() {
                    state.detach_frontend();
                }
            }
        })
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, tauri::WindowEvent::Destroyed) {
                if let Some(state) = window.try_state::<AppState>() {
                    state.detach_frontend();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            load_settings,
            save_settings,
            open_session,
            write_session,
            write_session_bytes,
            resize_session,
            ack_session,
            close_session,
            list_remote,
            download_file,
            upload_file,
            inspect_bookmarks,
            inspect_connection_file,
            export_connections
        ])
        .build(context)
        .expect("Could not start Relay");
    app.run(|app, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            if let Some(state) = app.try_state::<AppState>() {
                state.detach_frontend();
            }
        }
    });
}
