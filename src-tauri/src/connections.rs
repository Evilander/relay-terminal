use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_SETTINGS: u64 = 4 * 1024 * 1024;

pub fn lock_settings_directory(directory: &Path) -> Result<fs::File, String> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("settings.lock"))
        .map_err(|error| format!("Cannot open settings lock: {error}"))?;
    lock.try_lock()
        .map_err(|error| format!("Cannot exclusively lock Relay settings directory: {error}"))?;
    Ok(lock)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshProfile {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: Option<String>,
    pub jump_host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub profiles: Vec<SshProfile>,
    pub font_size: u16,
    pub scrollback: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: vec![],
            font_size: 14,
            scrollback: 10_000,
        }
    }
}

fn text(value: &str, label: &str, max: usize, empty: bool) -> Result<(), String> {
    if (!empty && value.is_empty()) || value.len() > max || value.chars().any(char::is_control) {
        return Err(format!("Invalid {label}"));
    }
    Ok(())
}

fn host(value: &str) -> Result<(), String> {
    text(value, "host", 253, false)?;
    if value.starts_with('-')
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_:[]%".contains(&c))
    {
        return Err("Host must be a hostname or IP address".into());
    }
    Ok(())
}

fn user(value: &str) -> Result<(), String> {
    text(value, "user", 128, false)?;
    if value.starts_with('-')
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-\\".contains(&c))
    {
        return Err("Invalid SSH user".into());
    }
    Ok(())
}

fn jump(value: &str) -> Result<(), String> {
    text(value, "jump host", 512, false)?;
    let server = if let Some((username, server)) = value.split_once('@') {
        user(username)?;
        server
    } else {
        value
    };
    if server.starts_with('[') {
        let end = server.find(']').ok_or("Invalid jump host IPv6 address")?;
        host(&server[..=end])?;
        if !server[end + 1..].is_empty() {
            let port = server[end + 1..]
                .strip_prefix(':')
                .ok_or("Invalid jump host port")?;
            if port.parse::<u16>().map_err(|_| "Invalid jump host port")? == 0 {
                return Err("Invalid jump host port".into());
            }
        }
    } else if let Some((server, port)) = server.rsplit_once(':') {
        host(server)?;
        if server.contains(':') || port.parse::<u16>().map_err(|_| "Invalid jump host port")? == 0 {
            return Err("Use [IPv6]:port for a jump host".into());
        }
    } else {
        host(server)?;
    }
    Ok(())
}

pub fn validate_profile(profile: &SshProfile) -> Result<(), String> {
    text(&profile.id, "profile ID", 128, false)?;
    text(&profile.name, "profile name", 256, false)?;
    text(&profile.folder, "folder", 1024, true)?;
    host(&profile.host)?;
    user(&profile.user)?;
    if profile.port == 0 {
        return Err("Port must be 1–65535".into());
    }
    if let Some(path) = &profile.key_path {
        text(path, "key path", 32768, false)?;
    }
    if let Some(value) = &profile.jump_host {
        jump(value)?;
    }
    Ok(())
}

pub fn validate_settings(settings: &Settings) -> Result<(), String> {
    if settings.profiles.len() > 1000
        || !(8..=48).contains(&settings.font_size)
        || !(100..=100_000).contains(&settings.scrollback)
    {
        return Err("Settings exceed supported limits".into());
    }
    let mut ids = HashSet::new();
    for profile in &settings.profiles {
        validate_profile(profile)?;
        if !ids.insert(&profile.id) {
            return Err("Profile IDs must be unique".into());
        }
    }
    Ok(())
}

pub fn load_settings(path: &Path) -> Result<Settings, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default())
        }
        Err(error) => return Err(format!("Cannot read settings: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SETTINGS {
        return Err("Settings exceed 4 MiB".into());
    }
    let settings: Settings = serde_json::from_slice(&bytes)
        .map_err(|_| "Settings file is invalid; it has not been overwritten")?;
    validate_settings(&settings)?;
    Ok(settings)
}

pub fn save_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    validate_settings(settings)?;
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SETTINGS {
        return Err("Settings exceed 4 MiB".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist(path)
        .map_err(|e| format!("Cannot replace settings: {}", e.error))?;
    Ok(())
}

pub struct SettingsStore {
    path: PathBuf,
    guard: Mutex<()>,
}
impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            guard: Mutex::new(()),
        }
    }
    pub fn load(&self) -> Result<Settings, String> {
        let _guard = self.guard.lock().map_err(|_| "Settings lock failed")?;
        load_settings(&self.path)
    }
    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        let _guard = self.guard.lock().map_err(|_| "Settings lock failed")?;
        save_settings(&self.path, settings)
    }
    pub fn profile(&self, id: &str) -> Result<SshProfile, String> {
        self.load()?
            .profiles
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| "Connection profile not found".into())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SshMode {
    Terminal,
    SftpTerminal,
    SftpSubsystem,
}

fn ssh_config_path(config: &Path) -> Result<String, String> {
    if !config.is_absolute() || !config.is_file() {
        return Err("RELAY_SSH_CONFIG must name an existing absolute file".into());
    }
    let value = config
        .to_str()
        .ok_or("RELAY_SSH_CONFIG is not valid Unicode")?;
    text(value, "SSH config path", 32768, false)?;
    Ok(value.to_owned())
}

pub fn build_ssh_args(profile: &SshProfile, mode: SshMode) -> Result<Vec<String>, String> {
    validate_profile(profile)?;
    let mut args = vec![
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=2".into(),
    ];
    // Operator-controlled fixture/config override; never accepted from a profile.
    if let Some(config) = std::env::var_os("RELAY_SSH_CONFIG") {
        let config = PathBuf::from(config);
        args.extend(["-F".into(), ssh_config_path(&config)?]);
    }
    if matches!(mode, SshMode::SftpSubsystem) {
        args.extend(
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
            ]
            .map(String::from),
        );
    }
    args.push(
        if matches!(mode, SshMode::SftpTerminal) {
            "-P"
        } else {
            "-p"
        }
        .into(),
    );
    args.push(profile.port.to_string());
    args.extend(["-o".into(), format!("User={}", profile.user)]);
    if let Some(key) = &profile.key_path {
        args.extend(["-i".into(), key.clone()]);
    }
    if let Some(jump) = &profile.jump_host {
        args.extend(["-J".into(), jump.clone()]);
    }
    if matches!(mode, SshMode::SftpSubsystem) {
        args.push("-s".into());
    }
    args.extend(["--".into(), profile.host.clone()]);
    if matches!(mode, SshMode::SftpSubsystem) {
        args.push("sftp".into());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_lock_excludes_other_processes_until_released() {
        if let Some(directory) = std::env::var_os("RELAY_TEST_LOCK_PROBE") {
            assert!(lock_settings_directory(Path::new(&directory)).is_err());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let lock = lock_settings_directory(directory.path()).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connections::tests::configuration_lock_excludes_other_processes_until_released",
            ])
            .env("RELAY_TEST_LOCK_PROBE", directory.path())
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(lock_settings_directory(directory.path()).is_err());
        drop(lock);
        assert!(lock_settings_directory(directory.path()).is_ok());
    }

    #[test]
    fn configuration_locks_do_not_block_other_directories() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let _first_lock = lock_settings_directory(first.path()).unwrap();
        assert!(lock_settings_directory(second.path()).is_ok());
    }
    pub fn profile() -> SshProfile {
        SshProfile {
            id: "one".into(),
            name: "Server".into(),
            folder: "".into(),
            host: "example.org".into(),
            port: 22,
            user: "alice".into(),
            key_path: None,
            jump_host: None,
        }
    }
    #[test]
    fn rejects_argument_and_control_injection() {
        for host in ["-oProxyCommand=bad", "ok\nwrong", "name;command", "a b"] {
            let mut p = profile();
            p.host = host.into();
            assert!(validate_profile(&p).is_err());
        }
        let mut p = profile();
        p.user = "-bad".into();
        assert!(validate_profile(&p).is_err());
        let mut p = profile();
        p.jump_host = Some("alice@jump;bad".into());
        assert!(validate_profile(&p).is_err());
    }
    #[test]
    fn subsystem_uses_strict_batch_auth_and_separate_arguments() {
        let args = build_ssh_args(&profile(), SshMode::SftpSubsystem).unwrap();
        assert!(args.iter().any(|a| a == "BatchMode=yes"));
        assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
        assert_eq!(&args[args.len() - 3..], &["--", "example.org", "sftp"]);
    }
    #[test]
    fn config_override_requires_an_existing_absolute_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            ssh_config_path(file.path()).unwrap(),
            file.path().to_str().unwrap()
        );
        assert!(ssh_config_path(Path::new("relative-config")).is_err());
        assert!(ssh_config_path(&file.path().with_extension("absent")).is_err());
        assert!(ssh_config_path(file.path().parent().unwrap()).is_err());
    }
    #[test]
    fn atomic_settings_roundtrip_and_invalid_save_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::new(path.clone());
        let mut settings = Settings::default();
        settings.profiles.push(profile());
        store.save(&settings).unwrap();
        assert_eq!(store.load().unwrap(), settings);
        let before = fs::read(&path).unwrap();
        settings.profiles.push(profile());
        assert!(store.save(&settings).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
