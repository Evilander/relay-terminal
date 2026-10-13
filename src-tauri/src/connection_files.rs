use crate::connections::{validate_settings, Settings, SshProfile};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

const MAX_CONNECTION_FILE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionFile {
    pub format: String,
    pub version: u32,
    pub profiles: Vec<SshProfile>,
}

impl ConnectionFile {
    fn validate(&self) -> Result<(), String> {
        if self.format != "relay.connections" || self.version != 1 {
            return Err("Unsupported connection file format or version".into());
        }
        validate_settings(&Settings {
            profiles: self.profiles.clone(),
            ..Settings::default()
        })
    }
}

pub fn inspect_connection_file(path: &Path) -> Result<ConnectionFile, String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("Choose an existing connection file with an absolute path".into());
    }
    let file =
        fs::File::open(path).map_err(|error| format!("Cannot read connection file: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_CONNECTION_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_CONNECTION_FILE {
        return Err("Connection file exceeds 4 MiB".into());
    }
    let connections: ConnectionFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid connection file: {error}"))?;
    connections.validate()?;
    Ok(connections)
}

pub fn export_connection_file(path: &Path, profiles: &[SshProfile]) -> Result<usize, String> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("Choose an absolute path for the connection file".into());
    }
    match fs::symlink_metadata(path) {
        Ok(_) => return Err("The destination already exists. Choose a new filename; existing files are never replaced.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(format!("Cannot inspect destination: {error}")),
    }
    let connections = ConnectionFile {
        format: "relay.connections".into(),
        version: 1,
        profiles: profiles.to_vec(),
    };
    connections.validate()?;
    let bytes = serde_json::to_vec_pretty(&connections).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_CONNECTION_FILE {
        return Err("Connection file exceeds 4 MiB".into());
    }
    let parent = path
        .parent()
        .ok_or("Connection file has no parent directory")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".relay-connections-")
        .tempfile_in(parent)
        .map_err(|error| format!("Cannot create connection file: {error}"))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist_noclobber(path).map_err(|error| {
        format!(
            "Cannot export without replacing an existing file: {}",
            error.error
        )
    })?;
    Ok(connections.profiles.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str) -> SshProfile {
        SshProfile {
            id: id.into(),
            name: "My server café".into(),
            folder: "Work".into(),
            host: "example.org".into(),
            port: 2222,
            user: "alice".into(),
            key_path: Some("C:\\Keys\\identity".into()),
            jump_host: Some("bob@bastion:22".into()),
        }
    }

    #[test]
    fn exports_profiles_only_and_roundtrips_without_replacing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let profiles = vec![profile("first")];
        assert_eq!(export_connection_file(&path, &profiles).unwrap(), 1);
        assert_eq!(inspect_connection_file(&path).unwrap().profiles, profiles);
        let before = fs::read(&path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(json.get("fontSize").is_none());
        assert!(export_connection_file(&path, &[])
            .unwrap_err()
            .contains("already exists"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn refuses_unknown_schema_versions_and_invalid_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        for contents in [
            r#"{"format":"other","version":1,"profiles":[]}"#,
            r#"{"format":"relay.connections","version":2,"profiles":[]}"#,
            r#"{"format":"relay.connections","version":1,"profiles":[],"password":"secret"}"#,
        ] {
            fs::write(&path, contents).unwrap();
            assert!(inspect_connection_file(&path).is_err());
        }
        let mut invalid = profile("first");
        invalid.host = "-oProxyCommand=bad".into();
        let file = ConnectionFile {
            format: "relay.connections".into(),
            version: 1,
            profiles: vec![invalid.clone()],
        };
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(inspect_connection_file(&path).is_err());
        assert!(export_connection_file(&dir.path().join("invalid.json"), &[invalid]).is_err());
        assert!(!dir.path().join("invalid.json").exists());
    }

    #[test]
    fn rejects_duplicate_ids_and_more_than_one_thousand_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        assert!(export_connection_file(&path, &[profile("same"), profile("same")]).is_err());
        let profiles: Vec<_> = (0..1001).map(|i| profile(&i.to_string())).collect();
        let file = ConnectionFile {
            format: "relay.connections".into(),
            version: 1,
            profiles,
        };
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(inspect_connection_file(&path).is_err());
    }

    #[test]
    fn bounds_import_and_export_size_without_creating_a_destination() {
        let dir = tempfile::tempdir().unwrap();
        let oversized = dir.path().join("oversized.json");
        fs::write(&oversized, vec![b' '; (MAX_CONNECTION_FILE + 1) as usize]).unwrap();
        assert!(inspect_connection_file(&oversized)
            .unwrap_err()
            .contains("4 MiB"));
        let profiles: Vec<_> = (0..600)
            .map(|i| {
                let mut p = profile(&i.to_string());
                p.key_path = Some("x".repeat(8192));
                p
            })
            .collect();
        let destination = dir.path().join("export.json");
        assert!(export_connection_file(&destination, &profiles)
            .unwrap_err()
            .contains("4 MiB"));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn requires_absolute_existing_inputs_and_preserves_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(inspect_connection_file(Path::new("connections.json")).is_err());
        assert!(inspect_connection_file(&dir.path().join("missing.json")).is_err());
        assert!(inspect_connection_file(dir.path()).is_err());
        assert!(export_connection_file(Path::new("connections.json"), &[]).is_err());
        assert!(export_connection_file(dir.path(), &[]).is_err());
        assert!(dir.path().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn preserves_dangling_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("connections.json");
        std::os::unix::fs::symlink("absent.json", &destination).unwrap();
        assert!(export_connection_file(&destination, &[]).is_err());
        assert_eq!(
            fs::read_link(destination).unwrap(),
            Path::new("absent.json")
        );
    }
}
