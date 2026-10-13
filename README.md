# Relay

Relay is a desktop workspace for local shells, SSH terminals and SFTP files. It uses Tauri, xterm.js and the system OpenSSH client.

[Download the Windows release](https://github.com/Evilander/relay-terminal/releases/latest).

On Windows, extract the ZIP and run `Relay.exe`, or use the setup executable. Builds are unsigned. WebView2 is required; the installer can install its runtime when missing. Launching Relay again focuses the existing window.

## Use the workspace

- Open local shells in tabs. The shell picker detects installed PowerShell, Command Prompt and WSL executables on Windows; WSL also needs a configured distribution.
- Save SSH profiles with a host, port, username, optional private-key path and jump host. Organize profiles in folders and find them with sidebar search.
- Back up profiles with **Export connections**, or use **Import connections** to preview a backup and add its new profiles. Matching connection fields are skipped; existing profiles and appearance settings are preserved.
- Search terminal output, copy selections, paste, change font size and reconnect sessions. Switching tabs keeps their terminals running; closing a tab closes its session.
- Open **Remote files** on an SSH tab to browse directories and upload or download individual files.
- Use **Inspect MobaXterm bookmarks** to read labels, folders and original opaque values from an INI or session file. This is a read-only inspector, not an importer or session converter.

X11, RDP and VNC sessions are not implemented. Windows is the current development target; macOS and Linux desktop builds and runtime behavior have not been validated.

## Run from source on Windows

Install these prerequisites:

- Node.js 22.12 or newer, with npm.
- Rust through rustup, using the MSVC Windows toolchain. The repository pins Rust 1.99.0 in `rust-toolchain.toml`.
- Visual Studio Build Tools with **Desktop development with C++** and a Windows SDK.
- Microsoft Edge WebView2 Runtime.
- Windows OpenSSH Client for SSH and SFTP (`ssh.exe` and `sftp.exe`). PowerShell 7 and WSL are optional.

See the [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/) for the compiler and WebView2 setup. From the repository directory:

```powershell
npm ci
npm run desktop
```

The desktop app opens the first detected local shell on startup. `npm run dev` serves a browser preview at `http://127.0.0.1:1420`; terminals, settings and local-file operations require the desktop app.

## SSH authentication and host trust

Create a profile with **New connection**, then choose **Save & connect**. The terminal uses OpenSSH: password prompts and key passphrase prompts appear there. Relay saves key paths, not private-key contents or passwords. A jump host can use the form `user@bastion:22`.

For a new server, connect in the terminal first and verify its host-key fingerprint through a trusted source before accepting it. OpenSSH normally records trusted keys in `%USERPROFILE%\.ssh\known_hosts`; SSH configuration can change that location.

The file browser starts a separate SSH connection with batch authentication and strict host-key checking. It requires an already trusted host and a key or SSH agent that can authenticate without a prompt. Entering a password in the terminal does not authenticate the file browser. Unlock encrypted keys in your SSH agent when needed. Host-key failures are shown as errors; Relay does not disable verification.

## File transfers

Uploads and downloads refuse an existing destination. Uploads keep the selected local filename: rename the local file or choose another remote directory if that name already exists. Downloads need an unused local path. Transfers use temporary files and publish the completed file without replacing an existing destination.

Each file is limited to 1 GiB, with a five-minute transfer timeout. Recursive directory transfers are not supported. A failed upload attempts to remove its temporary remote file; if cleanup fails, the error identifies the remaining path.

Transfers compare byte counts and file metadata before publishing their destinations. If a file changes or ends early, Relay rejects the transfer. This detects incomplete and changing files; it does not guarantee an immutable snapshot of a file being actively edited. If the server commits an upload but its reply is lost, the error identifies the destination to check before retrying.

Remote directory inventories and bookmark results are limited to 16 MiB each to keep large or malformed responses bounded.

## Connection backups

Exports contain profile names, hosts, usernames, ports, folders, private-key paths, and jump hosts. They contain no passwords or private-key contents. Treat a backup as connection metadata that you choose to share. Key paths are preserved as written; edit them after moving a backup to another machine.

Choose a new export filename: existing files are never replaced. Import accepts Relay connection JSON files up to 4 MiB and validates them before showing a preview. Confirmation adds only new connections with new local IDs; it does not connect to imported servers. At most 1,000 profiles can be saved.

## Settings

Profiles, font size and scrollback are stored in `settings.json` in the native application configuration directory. On Windows this is normally:

```text
%APPDATA%\com.evilander.relay\settings.json
```

Set `RELAY_CONFIG_DIR` to an absolute directory before launching to use another location. Close Relay before manually editing or restoring settings. If an existing settings file cannot be read or validated, saving is disabled to preserve it; local terminals can still run. Relay does not persist profiles in browser storage.

Close every tab for a profile and let its file operations finish before editing or deleting it, including tabs whose processes have exited.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+T | Open the default local shell |
| Ctrl+Shift+W | Close the active tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+Shift+F | Search terminal output |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy selection / paste, with terminal focus |
| Ctrl++ / Ctrl+- | Increase / decrease terminal font size |
| Enter / Shift+Enter / Escape | Next match / previous match / close search, with search focus |

## Build and test

```powershell
npm run build
npm test
node scripts/connection-ui-smoke.mjs .artifacts/connection-ui-check
npm run test:rust
npm run package
```

`build` checks TypeScript and builds the frontend. The connection UI check needs Microsoft Edge and exercises the real interface with mocked file pickers and native commands. `package` builds the desktop executable and platform bundles, normally under `src-tauri/target/release/` and `src-tauri/target/release/bundle/`. A custom `CARGO_TARGET_DIR` changes those locations.

The frontend is in `src/`; native commands, SSH/SFTP handling and terminal process management are in `src-tauri/src/`. The IPC contract is defined in `src/api.ts`.

Dependency versions are locked in `package-lock.json` and `src-tauri/Cargo.lock`. Regenerate the bundled license notices with `node scripts/generate-notices.mjs` after changing dependencies. Relay's own code is [MIT licensed](LICENSE); dependencies retain the terms in [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt).

For native integration checks, install `requirements-test.txt` and start `scripts/ssh-fixture.py` with an absolute, unused output directory and `--seconds 1200`. It creates temporary keys and a localhost SSH/SFTP server; it does not edit your SSH configuration. Use its printed `fixture.json` path with:

```powershell
cargo run --manifest-path src-tauri/Cargo.toml --example sftp_smoke -- C:\path\fixture.json
npm run tauri -- build --debug --no-bundle
node scripts/native-smoke.mjs src-tauri/target/debug/relay-terminal.exe C:\path\fixture.json .artifacts/native-check
```

The desktop test needs a fresh output directory and a Windows desktop session. It launches the real app with isolated settings, uses Playwright through WebView2, and closes its processes afterward. `RELAY_DEBUG_PORT` enables its loopback debugging connection only in debug builds; release builds do not read that variable. Native file-picker interactions and clipboard permissions still need manual platform testing.
