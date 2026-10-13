# Changelog

## 0.2.0

- Add connection backup and import with a preview, duplicate detection, and preservation of existing profiles.
- Focus the existing window when Relay is launched again, preventing competing settings writes.
- Reject SFTP downloads that end early or change during transfer, and uploads whose source or remote size changes.
- Report an uncertain upload destination when the server commits a file but its acknowledgment is lost.
- Bound the size of remote directory listings and bookmark inventories, including repeated metadata and JSON escaping.
- Keep private-key picker results with their original connection dialog and prevent dismissal during settings saves.
- Clear stale bookmark results after failed inspections.
- Ensure finite test helpers clean up their owned process trees when a timeout or artifact-write error occurs.

The Windows build provides local terminal tabs, SSH profiles, SFTP browsing and file transfers, terminal search, and read-only MobaXterm bookmark inspection. X11, RDP, and VNC are not implemented. macOS and Linux runtime behavior remains unvalidated.
