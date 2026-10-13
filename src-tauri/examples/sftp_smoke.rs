use relay_terminal_lib::{connections::SshProfile, sftp};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    profile: SshProfile,
    ssh_config: String,
    root: PathBuf,
    payload_sha256: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_path =
        PathBuf::from(std::env::args_os().nth(1).ok_or("Pass fixture.json")?).canonicalize()?;
    let fixture: Fixture = serde_json::from_slice(&std::fs::read(&fixture_path)?)?;
    let directory = fixture_path.parent().ok_or("Fixture needs a parent")?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let results = directory.join(format!("run-{run_id}"));
    std::fs::create_dir(&results)?;
    let remote_upload = format!("/uploaded café-{run_id}.bin");
    std::env::set_var("RELAY_SSH_CONFIG", &fixture.ssh_config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listing = sftp::list_remote(&fixture.profile, ".").await?;
        assert!(listing.entries.iter().any(|entry| entry.name == "hello café.txt"));
        assert!(listing.entries.iter().any(|entry| entry.name == "nested" && entry.is_dir));
        let download = results.join("download.bin");
        let received = sftp::download_file(&fixture.profile, "/payload.bin", &download).await?;
        let downloaded = std::fs::read(&download).map_err(|error| error.to_string())?;
        assert_eq!(format!("{:x}", Sha256::digest(&downloaded)), fixture.payload_sha256);
        assert!(sftp::download_file(&fixture.profile, "/payload.bin", &download).await.is_err());
        assert_eq!(std::fs::read(&download).map_err(|e| e.to_string())?, downloaded);
        let upload = directory.join("upload.bin");
        let sent = sftp::upload_file(&fixture.profile, &upload, &remote_upload).await?;
        let upload_bytes = std::fs::read(&upload).map_err(|e| e.to_string())?;
        assert_eq!(std::fs::read(fixture.root.join(remote_upload.trim_start_matches('/'))).map_err(|e| e.to_string())?, upload_bytes);
        assert!(sftp::upload_file(&fixture.profile, &upload, &remote_upload).await.is_err());
        assert_eq!(std::fs::read(fixture.root.join(remote_upload.trim_start_matches('/'))).map_err(|e| e.to_string())?, upload_bytes);
        let absent = sftp::download_file(&fixture.profile, "/missing", &results.join("missing.bin")).await;
        assert!(absent.is_err());
        assert!(!results.join("missing.bin").exists());
        assert!(!std::fs::read_dir(&fixture.root).map_err(|e| e.to_string())?.filter_map(Result::ok).any(|entry| entry.file_name().to_string_lossy().starts_with(".relay-")));
        let hidden = sftp::list_remote(&fixture.profile, "/.hidden").await?;
        assert_eq!(hidden.path, "/.hidden");
        assert!(hidden.entries.iter().any(|entry| entry.name == "visible.txt"));
        for name in ["premature", "changed"] {
            let destination = results.join(format!("{name}.bin"));
            let error = sftp::download_file(&fixture.profile, &format!("/{name}.bin"), &destination).await.unwrap_err();
            assert!(error.contains("changed or ended early"), "{error}");
            assert!(!destination.exists());
        }
        let unstable = directory.join("unstable-upload.bin");
        std::fs::write(&unstable, &upload_bytes).map_err(|e| e.to_string())?;
        let mutation_remote = format!("/mutating-upload/result-{run_id}.bin");
        let mutation_error = sftp::upload_file(&fixture.profile, &unstable, &mutation_remote).await.unwrap_err();
        assert!(mutation_error.contains("Local file changed or ended early"), "{mutation_error}");
        assert!(!fixture.root.join(mutation_remote.trim_start_matches('/')).exists());
        assert!(std::fs::read_dir(fixture.root.join("mutating-upload")).map_err(|e| e.to_string())?.next().is_none());
        let inventory_error = sftp::list_remote(&fixture.profile, "/virtual-large-list").await.unwrap_err();
        assert!(inventory_error.contains("16 MiB"), "{inventory_error}");
        let uncertain_remote = format!("/lost-ack-{run_id}.bin");
        let uncertain_error = sftp::upload_file(&fixture.profile, &upload, &uncertain_remote).await.unwrap_err();
        assert!(uncertain_error.contains("commit was not acknowledged") && uncertain_error.contains(&uncertain_remote), "{uncertain_error}");
        assert_eq!(std::fs::read(fixture.root.join(uncertain_remote.trim_start_matches('/'))).map_err(|e| e.to_string())?, upload_bytes);
        let untrusted = results.join("untrusted_config");
        let source_config = std::fs::read_to_string(&fixture.ssh_config).map_err(|e| e.to_string())?;
        let untrusted_config = source_config.lines().map(|line| {
            if line.trim_start().starts_with("UserKnownHostsFile ") {
                "  UserKnownHostsFile none"
            } else { line }
        }).collect::<Vec<_>>().join("\n");
        std::fs::write(&untrusted, untrusted_config).map_err(|e| e.to_string())?;
        std::env::set_var("RELAY_SSH_CONFIG", &untrusted);
        let rejection = sftp::list_remote(&fixture.profile, ".").await.unwrap_err();
        std::env::set_var("RELAY_SSH_CONFIG", &fixture.ssh_config);
        assert!(rejection.contains("Host key verification failed"), "{rejection}");
        let proof = serde_json::json!({"listingEntries":listing.entries.len(),"unicodeName":true,"downloadBytes":received.bytes,"uploadBytes":sent.bytes,"sha256Verified":true,"localCollisionPreserved":true,"remoteCollisionPreserved":true,"missingDownloadCleaned":true,"remoteTemporaryFilesCleaned":true,"untrustedHostRejected":true,"hiddenDirectoryPreserved":true,"prematureEOFRejected":true,"changedRemoteFileRejected":true,"changedUploadSourceRejected":true,"largeDirectoryRejected":true,"uncertainCommitReportedWithoutDeletingDestination":true});
        std::fs::write(directory.join("sftp-proof.json"), serde_json::to_vec_pretty(&proof).unwrap()).map_err(|e| e.to_string())?;
        println!("{proof}");
        Ok::<_, String>(())
    })?;
    Ok(())
}
