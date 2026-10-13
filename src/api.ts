import { Channel, invoke, isTauri } from '@tauri-apps/api/core';

export interface SshProfile {
  id: string; name: string; folder: string; host: string; port: number;
  user: string; keyPath: string | null; jumpHost: string | null;
}
export interface ShellInfo { id: string; name: string; path: string }
export interface AppInfo { version: string; platform: string; home: string; shells: ShellInfo[]; sshAvailable: boolean; sftpAvailable: boolean }
export interface Settings { profiles: SshProfile[]; fontSize: number; scrollback: number }
export interface OpenRequest { id: string; kind: 'local' | 'ssh' | 'sftp'; shell?: string; cwd?: string; profileId?: string; cols: number; rows: number }
export type TerminalEvent =
  | { type: 'data'; id: string; seq: number; data: number[] }
  | { type: 'exit'; id: string; code: number | null }
  | { type: 'error'; id: string; message: string };
export interface SessionInfo { id: string; pid: number | null; title: string }
export interface RemoteEntry { name: string; path: string; isDir: boolean; isSymlink: boolean; size: number; modified: number | null }
export interface RemoteListing { path: string; entries: RemoteEntry[] }
export interface TransferResult { bytes: number; localPath: string; remotePath: string }
export interface BookmarkEntry { section: string; folder: string; name: string; rawValue: string; image: string; ignoredBySource: boolean }
export interface BookmarkInventory { entries: BookmarkEntry[]; warnings: string[]; source: string }
export interface ConnectionFile { format: 'relay.connections'; version: 1; profiles: SshProfile[] }

export const native = isTauri();
export const api = {
  appInfo: () => invoke<AppInfo>('app_info'),
  loadSettings: () => invoke<Settings>('load_settings'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  open: (request: OpenRequest, receive: (event: TerminalEvent) => void) => {
    const onEvent = new Channel<TerminalEvent>();
    onEvent.onmessage = receive;
    return invoke<SessionInfo>('open_session', { request, onEvent });
  },
  write: (id: string, data: string) => invoke<void>('write_session', { id, data }),
  writeBytes: (id: string, data: number[]) => invoke<void>('write_session_bytes', { id, data }),
  resize: (id: string, cols: number, rows: number) => invoke<void>('resize_session', { id, cols, rows }),
  ack: (id: string, seq: number) => invoke<void>('ack_session', { id, seq }),
  close: (id: string) => invoke<void>('close_session', { id }),
  listRemote: (profileId: string, path: string) => invoke<RemoteListing>('list_remote', { profileId, path }),
  download: (profileId: string, remotePath: string, localPath: string) => invoke<TransferResult>('download_file', { profileId, remotePath, localPath }),
  upload: (profileId: string, localPath: string, remotePath: string) => invoke<TransferResult>('upload_file', { profileId, localPath, remotePath }),
  inspectBookmarks: (path: string) => invoke<BookmarkInventory>('inspect_bookmarks', { path }),
  inspectConnections: (path: string) => invoke<ConnectionFile>('inspect_connection_file', { path }),
  exportConnections: (path: string) => invoke<number>('export_connections', { path }),
};
