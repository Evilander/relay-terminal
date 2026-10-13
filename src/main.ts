import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { open as pickOpen, save as pickSave } from '@tauri-apps/plugin-dialog';
import { api, native, type AppInfo, type Settings, type SshProfile, type OpenRequest, type TerminalEvent, type RemoteEntry, type BookmarkInventory, type ConnectionFile } from './api';
import { SerialQueue, SessionLifetime, ProfileActivity, remoteJoin, remoteParent, formatBytes, previewConnectionImport, mergeConnectionImport } from './ui-state';
import '@xterm/xterm/css/xterm.css';
import './style.css';

const icons: Record<string, string> = {
  terminal: '<path d="m4 6 6 6-6 6m9 0h7"/>', plus: '<path d="M12 5v14M5 12h14"/>',
  close: '<path d="m6 6 12 12M18 6 6 18"/>', search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4 4"/>',
  server: '<rect x="4" y="3" width="16" height="7" rx="2"/><rect x="4" y="14" width="16" height="7" rx="2"/><path d="M8 6.5h.01M8 17.5h.01"/>',
  folder: '<path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/>',
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Zm0 0v6h6"/>',
  refresh: '<path d="M20 7v5h-5M4 17v-5h5"/><path d="M6 6a8 8 0 0 1 13 3M18 18A8 8 0 0 1 5 15"/>',
  copy: '<rect x="8" y="8" width="12" height="13" rx="2"/><path d="M16 8V3H3v13h5"/>',
  paste: '<path d="M8 5H4v16h16V5h-4"/><rect x="8" y="2" width="8" height="5" rx="1"/>',
  upload: '<path d="m7 9 5-5 5 5M12 4v12M4 16v5h16v-5"/>',
  download: '<path d="m7 11 5 5 5-5M12 3v13M4 16v5h16v-5"/>',
  chevron: '<path d="m8 10 4 4 4-4"/>', up: '<path d="m6 15 6-6 6 6"/>',
  edit: '<path d="m15 4 5 5M4 20l5-1L21 7a2 2 0 0 0-5-5L4 14Z"/>',
  book: '<path d="M4 3h14a2 2 0 0 1 2 2v16H6a2 2 0 0 1-2-2Zm0 14h16M8 3v7l3-2 3 2V3"/>',
  link: '<path d="m10 13 4-4M8 16l-1 1a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0M13 7l1-1a4 4 0 0 1 6 6l-4 4a4 4 0 0 1-6 0"/>',
};
const svg = (name: string) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name] || icons.terminal}</svg>`;
const $ = <T extends HTMLElement = HTMLElement>(selector: string) => document.querySelector<T>(selector)!;
const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text = '') => {
  const node = document.createElement(tag); node.className = className; node.textContent = text; return node;
};
function action(label: string, icon: string, handler: () => void, className = 'icon-button') {
  const b = el('button', className); b.type = 'button'; b.title = label; b.setAttribute('aria-label', label);
  b.innerHTML = svg(icon); b.addEventListener('click', handler); return b;
}
function errorText(error: unknown): string { return error instanceof Error ? error.message : String(error); }

$('#app').innerHTML = `
  <aside class="sidebar" aria-label="Connections">
    <div class="brand"><span class="brand-mark">${svg('terminal')}</span><span>relay<span class="brand-period">.</span></span><span class="brand-tag">TERMINAL</span></div>
    <button class="button primary new-session" id="new-session">${svg('plus')}<span>New connection</span><span class="new-session-type">SSH</span></button>
    <label class="profile-search">${svg('search')}<input id="profile-filter" type="search" placeholder="Find a connection…" aria-label="Search connections"/></label>
    <div class="section-heading"><span>YOUR CONNECTIONS</span><span id="profile-count">0</span></div>
    <div id="profiles" class="profile-list"></div>
    <div class="sidebar-tools"><div class="connection-file-tools"><button id="import-connections" class="text-button">${svg('download')}<span>Import connections</span></button><button id="export-connections" class="text-button">${svg('upload')}<span>Export</span></button></div><button id="inspect-bookmarks" class="text-button">${svg('book')}<span>Inspect MobaXterm bookmarks</span></button></div>
    <footer class="sidebar-footer"><span class="platform-indicator"></span><div><strong id="platform-label">Loading workspace</strong><span id="version-label">Local settings · no cloud account</span></div></footer>
  </aside>
  <main class="workspace">
    <header class="workspace-header"><div class="workspace-heading"><span class="eyebrow">WORKSPACE</span><strong id="workspace-title">Your next connection starts here</strong></div><div class="header-actions"><span id="native-badge" class="badge">STARTING</span><button id="shell-button" class="button secondary">${svg('terminal')}<span>Local shell</span>${svg('chevron')}</button></div></header>
    <div class="session-strip"><div id="tabs" class="tabs" role="tablist" aria-label="Terminal sessions"></div><button id="add-tab" class="icon-button" aria-label="Open a local shell" title="Open a local shell (Ctrl+Shift+T)">${svg('plus')}</button></div>
    <div class="terminal-toolbar"><div class="session-context"><span id="session-indicator" class="status-dot"></span><span id="session-context">No active session</span></div><div id="terminal-actions" class="toolbar-actions"></div></div>
    <div id="session-message" class="session-message hidden" role="status"></div>
    <div class="search-bar hidden" id="terminal-search"><label>${svg('search')}<input id="terminal-search-input" aria-label="Find in terminal" placeholder="Find in terminal…" /></label><span id="search-result" aria-live="polite"></span><button id="search-previous" class="icon-button" aria-label="Previous match">${svg('up')}</button><button id="search-next" class="icon-button" aria-label="Next match">${svg('chevron')}</button><button id="search-close" class="icon-button" aria-label="Close search">${svg('close')}</button></div>
    <div class="work-area">
      <section id="terminal-area" class="terminal-area" aria-label="Terminal workspace">
        <div id="empty-state" class="empty-state"><div class="empty-emblem">${svg('terminal')}</div><span class="eyebrow">A CLEAR PATH TO YOUR MACHINES</span><h1>Make yourself<br/><span>at the command line.</span></h1><p>Local shells and remote connections,<br/>together in one focused workspace.</p><div class="empty-actions"><button id="empty-shell" class="button primary">${svg('terminal')}Open a local shell</button><button id="empty-ssh" class="button secondary">${svg('plus')}Add SSH connection</button></div><div id="empty-note" class="empty-note"></div><div class="shortcut-guide"><span><kbd>Ctrl Shift T</kbd> New shell</span><span><kbd>Ctrl Tab</kbd> Switch tabs</span><span><kbd>Ctrl Shift F</kbd> Find</span></div></div>
        <div id="terminal-hosts"></div>
      </section>
      <aside id="file-panel" class="file-panel hidden" aria-label="Remote files"><header><div><span class="eyebrow">SFTP</span><strong>Remote files</strong></div><button id="files-close" class="icon-button" aria-label="Close file browser">${svg('close')}</button></header><form id="remote-path-form" class="remote-path"><input id="remote-path" aria-label="Remote directory" placeholder="Remote directory" autocomplete="off"/><button class="icon-button" type="submit" aria-label="Open remote directory">${svg('chevron')}</button></form><div class="file-actions"><button id="remote-up" class="text-button">${svg('up')}Up</button><button id="remote-refresh" class="text-button">${svg('refresh')}Refresh</button><button id="remote-upload" class="text-button">${svg('upload')}Upload</button></div><div id="remote-status" class="remote-status" role="status"></div><div id="remote-files" class="remote-files"></div><footer>Files are transferred using your SSH profile.</footer></aside>
    </div>
    <footer class="statusbar"><span id="status-left">Ready</span><span id="status-right">UTF-8<span class="status-separator">/</span>Relay</span></footer>
  </main>
  <div id="toast" class="toast hidden" role="status" aria-live="polite"></div>
  <dialog id="connection-dialog" class="dialog connection-dialog" aria-labelledby="connection-title">
    <form id="connection-form"><header class="dialog-heading"><div><span class="eyebrow">SSH CONNECTION</span><h2 id="connection-title">A place to connect.</h2></div><button id="connection-close" type="button" class="icon-button" aria-label="Close connection dialog">${svg('close')}</button></header>
    <div class="form-grid"><label class="full">Connection name<input name="name" required maxlength="100" placeholder="My server" autocomplete="off"/></label><label>Host<input name="host" required maxlength="255" placeholder="hostname or IP address" autocomplete="off" spellcheck="false"/></label><label class="port-field">Port<input name="port" type="number" required min="1" max="65535" value="22"/></label><label>Username<input name="user" required maxlength="100" placeholder="Your remote user" autocomplete="off" spellcheck="false"/></label><label>Folder<input name="folder" maxlength="100" placeholder="Optional grouping" autocomplete="off"/></label><label class="full">Private key <span class="optional">optional</span><span class="input-with-button"><input name="keyPath" placeholder="Use SSH agent or default keys" autocomplete="off" spellcheck="false"/><button id="pick-key" type="button" class="icon-button" aria-label="Choose a private key file">${svg('folder')}</button></span></label><label class="full">Jump host <span class="optional">optional</span><input name="jumpHost" placeholder="user@bastion:22" autocomplete="off" spellcheck="false"/></label></div>
    <p class="form-note">Passwords are never saved in profiles. Authenticate in the terminal when prompted, or use an SSH agent or key.</p><p id="connection-error" class="form-error" role="alert"></p><footer class="dialog-footer"><button id="delete-profile" type="button" class="button danger hidden">Delete profile</button><span class="spacer"></span><button name="intent" value="save" class="button secondary" type="submit">Save profile</button><button name="intent" value="connect" class="button primary" type="submit">Save & connect ${svg('link')}</button></footer></form>
  </dialog>
  <dialog id="shell-dialog" class="dialog shell-dialog" aria-labelledby="shell-title"><header class="dialog-heading"><div><span class="eyebrow">ON THIS COMPUTER</span><h2 id="shell-title">Choose your shell.</h2></div><button id="shell-close" class="icon-button" aria-label="Close shell picker">${svg('close')}</button></header><div id="shell-list" class="shell-list"></div><p class="form-note" id="shell-note"></p></dialog>
  <dialog id="bookmark-dialog" class="dialog bookmark-dialog" aria-labelledby="bookmark-title"><header class="dialog-heading"><div><span class="eyebrow">READ-ONLY INVENTORY</span><h2 id="bookmark-title">MobaXterm bookmarks</h2></div><button id="bookmarks-close" class="icon-button" aria-label="Close bookmark inventory">${svg('close')}</button></header><div class="bookmark-controls"><button id="pick-bookmarks" class="button secondary">${svg('folder')}Choose INI or session file</button><input id="bookmark-filter" type="search" placeholder="Filter bookmarks…" aria-label="Filter bookmarks"/></div><p id="bookmark-source" class="form-note">Inspect folder names, labels and original values. This does not convert or launch sessions.</p><div id="bookmark-warnings" class="bookmark-warnings" role="status"></div><div class="bookmark-content"><div id="bookmark-list" class="bookmark-list"></div><label class="bookmark-detail">Original value<textarea id="bookmark-value" readonly spellcheck="false" placeholder="Choose a bookmark to inspect its opaque value."></textarea></label></div></dialog>
  <dialog id="import-preview-dialog" class="dialog import-dialog" aria-labelledby="import-title"><header class="dialog-heading"><div><span class="eyebrow">CONNECTION FILE</span><h2 id="import-title">Import connections</h2></div><button id="import-close" class="icon-button" aria-label="Close import preview">${svg('close')}</button></header><p id="import-source" class="form-note"></p><p id="import-summary" class="import-summary" role="status"></p><div id="import-list" class="import-list"></div><p class="form-note">Existing connections stay as they are. Matching endpoints are skipped. Imported connections receive new IDs and are saved without opening a session. Key paths may need updating on this computer.</p><p id="import-error" class="form-error" role="alert"></p><footer class="dialog-footer"><span class="spacer"></span><button id="import-cancel" class="button secondary">Cancel</button><button id="confirm-import" class="button primary">Add connections</button></footer></dialog>
`;

interface WorkspaceSession {
  id: string; request: OpenRequest; title: string; profile?: SshProfile;
  term: Terminal; fit: FitAddon; search: SearchAddon; host: HTMLDivElement;
  state: 'opening' | 'live' | 'exited' | 'error'; detail: string;
  lifetime: SessionLifetime; releaseProfile: () => void; queue: SerialQueue; cols: number; rows: number;
  inputReady: Promise<void>; releaseInput: () => void;
}
const sessions = new Map<string, WorkspaceSession>();
let activeId: string | null = null;
let appInfo: AppInfo | null = null;
let settings: Settings = { profiles: [], fontSize: 14, scrollback: 10000 };
let ready = false;
let settingsReady = false;
const settingsQueue = new SerialQueue();
const profileActivity = new ProfileActivity();
let editId: string | null = null;
let connectionGeneration = 0;
let connectionBusy = false;
let toastTimer = 0;
let filePanelOpen = false;
let remoteGeneration = 0;
let remotePath = '.';
let remoteEntries: RemoteEntry[] = [];
let remoteProfileId: string | null = null;
let remoteBusy = false;
let bookmarks: BookmarkInventory | null = null;
let bookmarkGeneration = 0;
let pendingConnections: ConnectionFile | null = null;
let importBusy = false;

const active = () => activeId ? sessions.get(activeId) : undefined;
function notify(message: string, error = false) {
  window.clearTimeout(toastTimer); const toast = $('#toast'); toast.textContent = message;
  toast.classList.remove('hidden'); toast.classList.toggle('error', error);
  toastTimer = window.setTimeout(() => toast.classList.add('hidden'), error ? 9000 : 5000);
}
function desktopRequired(): boolean {
  if (native && ready) return true;
  notify(native ? 'The native workspace is not ready. See the status below.' : 'Open Relay desktop to use shells, SSH profiles and local files.', true); return false;
}
async function persist(change: (current: Settings) => Settings) {
  if (!desktopRequired()) throw new Error('Settings are available in the desktop app.');
  if (!settingsReady) throw new Error('Settings could not be loaded. Saving is disabled to preserve the existing file.');
  try { await settingsQueue.run(async () => {
    const next = change(structuredClone(settings));
    await api.saveSettings(next);
    settings = structuredClone(next);
    renderProfiles();
  }); }
  catch (error) { notify(`Settings were not saved: ${errorText(error)}`, true); throw error; }
}

function renderProfiles() {
  const holder = $('#profiles'); holder.replaceChildren();
  $('#profile-count').textContent = String(settings.profiles.length);
  const query = $<HTMLInputElement>('#profile-filter').value.trim().toLowerCase();
  const profiles = settings.profiles.filter(p => `${p.name} ${p.host} ${p.user} ${p.folder}`.toLowerCase().includes(query));
  if (!profiles.length) {
    const empty = el('div', 'profiles-empty'); empty.append(el('strong', '', query ? 'No connections found' : 'Room for your connections'), el('p', '', query ? 'Try another name, host or folder.' : 'Save an SSH profile to keep it within reach.')); holder.append(empty); return;
  }
  const folders = new Map<string, SshProfile[]>();
  for (const profile of profiles) { const group = profile.folder || 'Ungrouped'; folders.set(group, [...(folders.get(group) || []), profile]); }
  for (const [name, rows] of folders) {
    const section = el('section', 'profile-group'); const heading = el('div', 'folder-heading'); heading.innerHTML = svg('folder'); heading.append(el('span', '', name), el('span', 'folder-count', String(rows.length))); section.append(heading);
    for (const p of rows) {
      const row = el('div', 'profile-row'); const connect = el('button', 'profile-connect'); connect.type = 'button'; connect.title = `Connect to ${p.user}@${p.host}:${p.port}`;
      const glyph = el('span', 'profile-icon'); glyph.innerHTML = svg('server');
      const label = el('span', 'profile-label'); label.append(el('strong', '', p.name), el('span', '', `${p.user}@${p.host}`)); connect.append(glyph, label);
      connect.addEventListener('click', () => openSsh(p));
      row.append(connect, action(`Edit ${p.name}`, 'edit', () => showConnection(p), 'icon-button profile-edit')); section.append(row);
    }
    holder.append(section);
  }
}

function showConnection(profile?: SshProfile) {
  if (connectionBusy) { notify('Wait for the connection save to finish.', true); return; }
  if (profile && profileActivity.isInUse(profile.id)) {
    notify('Close every tab for this profile and wait for its file operations to finish before editing it.', true); return;
  }
  editId = profile?.id ?? null;
  connectionGeneration++;
  const form = $<HTMLFormElement>('#connection-form'); form.reset();
  for (const [key, value] of Object.entries(profile ?? {})) { const input = form.elements.namedItem(key); if (input instanceof HTMLInputElement) input.value = value === null ? '' : String(value); }
  $('#connection-title').textContent = profile ? 'Edit connection.' : 'A place to connect.';
  $('#delete-profile').classList.toggle('hidden', !profile); $('#connection-error').textContent = native ? '' : 'Profile saving and connections are available in Relay desktop.';
  $<HTMLDialogElement>('#connection-dialog').showModal(); (form.elements.namedItem('name') as HTMLInputElement).focus();
}

$<HTMLFormElement>('#connection-form').addEventListener('submit', async event => {
  event.preventDefault(); const form = event.currentTarget as HTMLFormElement;
  if (!form.reportValidity()) return;
  const values = new FormData(form); const get = (name: string) => String(values.get(name) ?? '').trim();
  const profile: SshProfile = { id: editId ?? crypto.randomUUID(), name: get('name'), folder: get('folder'), host: get('host'), port: Number(get('port')), user: get('user'), keyPath: get('keyPath') || null, jumpHost: get('jumpHost') || null };
  if (!profile.name || !profile.host || !profile.user || /[\r\n\x00]/.test(Object.values(profile).join(''))) { $('#connection-error').textContent = 'Name, host and username are required and must not contain control characters.'; return; }
  const controls = [...form.querySelectorAll<HTMLInputElement | HTMLButtonElement>('input,button')]; controls.forEach(control => control.disabled = true);
  connectionBusy = true;
  let releaseProfile = () => {};
  try {
    releaseProfile = profileActivity.mutate(profile.id);
    await persist(current => ({ ...current, profiles: [...current.profiles.filter(p => p.id !== profile.id), profile] }));
    resetRemote();
    releaseProfile();
    $<HTMLDialogElement>('#connection-dialog').close();
    if ((event.submitter as HTMLButtonElement | null)?.value === 'connect') openSsh(profile); else notify('Connection saved.');
  } catch (error) { $('#connection-error').textContent = errorText(error); }
  finally { releaseProfile(); connectionBusy = false; controls.forEach(control => control.disabled = false); }
});
$('#delete-profile').addEventListener('click', async () => {
  if (!editId || !window.confirm('Delete this saved connection?')) return;
  const id = editId;
  connectionBusy = true;
  const controls = [...$('#connection-form').querySelectorAll<HTMLInputElement | HTMLButtonElement>('input,button')]; controls.forEach(control => control.disabled = true);
  let releaseProfile = () => {};
  try { releaseProfile = profileActivity.mutate(id); await persist(current => ({ ...current, profiles: current.profiles.filter(p => p.id !== id) })); resetRemote(); $<HTMLDialogElement>('#connection-dialog').close(); }
  catch (error) { $('#connection-error').textContent = errorText(error); }
  finally { releaseProfile(); connectionBusy = false; controls.forEach(control => control.disabled = false); }
});
$('#pick-key').addEventListener('click', async () => {
  if (!desktopRequired()) return;
  const generation = connectionGeneration;
  const ownsDialog = () => generation === connectionGeneration && $<HTMLDialogElement>('#connection-dialog').open;
  try { const path = await pickOpen({ title: 'Choose an SSH private key', multiple: false, directory: false }); if (typeof path === 'string' && ownsDialog() && !connectionBusy) ($<HTMLFormElement>('#connection-form').elements.namedItem('keyPath') as HTMLInputElement).value = path; }
  catch (error) { if (ownsDialog()) $('#connection-error').textContent = errorText(error); }
});
$<HTMLDialogElement>('#connection-dialog').addEventListener('cancel', event => { if (connectionBusy) event.preventDefault(); });

function renderImportPreview() {
  if (!pendingConnections) return;
  const preview = previewConnectionImport(settings.profiles, pendingConnections.profiles);
  $('#import-summary').textContent = `${preview.additions.length} new connections · ${preview.duplicates} duplicates skipped`;
  const additions = new Set(preview.additions);
  $('#import-list').replaceChildren(...pendingConnections.profiles.map(profile => {
    const row = el('div', 'import-entry');
    const label = el('div'); label.append(el('strong', '', profile.name), el('span', '', `${profile.user}@${profile.host}:${profile.port}${profile.folder ? ` · ${profile.folder}` : ''}`));
    row.append(label, el('span', additions.has(profile) ? 'import-new' : 'import-duplicate', additions.has(profile) ? 'New' : 'Skipped'));
    return row;
  }));
  const overCapacity = settings.profiles.length + preview.additions.length > 1000;
  $('#import-error').textContent = overCapacity ? 'Import would exceed the limit of 1,000 saved connections. Remove some profiles or choose a smaller file.' : '';
  $<HTMLButtonElement>('#confirm-import').disabled = importBusy || overCapacity || preview.additions.length === 0;
}

$('#import-connections').addEventListener('click', async () => {
  if (!desktopRequired()) return;
  if (!settingsReady) { notify('Settings could not be loaded. Import is disabled to preserve the existing file.', true); return; }
  const button = $<HTMLButtonElement>('#import-connections'); button.disabled = true;
  try {
    const path = await pickOpen({ title: 'Import Relay connections', directory: false, multiple: false, filters: [{ name: 'Relay connection file', extensions: ['json'] }] });
    if (typeof path !== 'string') return;
    pendingConnections = await api.inspectConnections(path);
    $('#import-source').textContent = path;
    renderImportPreview(); $<HTMLDialogElement>('#import-preview-dialog').showModal();
  } catch (error) { notify(`Connections were not imported: ${errorText(error)}`, true); }
  finally { button.disabled = false; }
});

$('#confirm-import').addEventListener('click', async () => {
  if (!pendingConnections || importBusy) return;
  const incoming = pendingConnections.profiles;
  let added = 0; let duplicates = 0;
  importBusy = true;
  const buttons = [...$('#import-preview-dialog').querySelectorAll<HTMLButtonElement>('button')]; buttons.forEach(b => b.disabled = true);
  try {
    await persist(current => {
      const merged = mergeConnectionImport(current, incoming);
      added = merged.added; duplicates = merged.duplicates;
      return merged.settings;
    });
    $<HTMLDialogElement>('#import-preview-dialog').close();
    notify(`Added ${added} connections. Skipped ${duplicates} duplicates.`);
  } catch (error) { $('#import-error').textContent = errorText(error); }
  finally { importBusy = false; buttons.forEach(b => b.disabled = false); }
});

$('#export-connections').addEventListener('click', async () => {
  if (!desktopRequired()) return;
  if (!settingsReady) { notify('Settings could not be loaded. Export is disabled.', true); return; }
  const button = $<HTMLButtonElement>('#export-connections'); button.disabled = true;
  try {
    const path = await pickSave({ title: 'Export Relay connections', defaultPath: `relay-connections-${new Date().toISOString().slice(0, 10)}.json`, filters: [{ name: 'Relay connection file', extensions: ['json'] }] });
    if (!path) return;
    const count = await settingsQueue.run(() => api.exportConnections(path));
    notify(`Exported ${count} connections to ${path}. Private keys and passwords are not included.`);
  } catch (error) { notify(`Connections were not exported: ${errorText(error)}`, true); }
  finally { button.disabled = false; }
});

$<HTMLDialogElement>('#import-preview-dialog').addEventListener('cancel', event => { if (importBusy) event.preventDefault(); });
$<HTMLDialogElement>('#import-preview-dialog').addEventListener('close', () => { pendingConnections = null; });
for (const selector of ['#import-close', '#import-cancel']) $(selector).addEventListener('click', () => $<HTMLDialogElement>('#import-preview-dialog').close());

function showShells() {
  const list = $('#shell-list'); list.replaceChildren();
  for (const shell of appInfo?.shells || []) {
    const button = el('button', 'shell-choice'); button.innerHTML = svg('terminal'); const label = el('span'); label.append(el('strong', '', shell.name), el('small', '', shell.path)); button.append(label);
    button.addEventListener('click', () => { $<HTMLDialogElement>('#shell-dialog').close(); openSession({ kind: 'local', shell: shell.id }, shell.name); }); list.append(button);
  }
  $('#shell-note').textContent = !native ? 'Local shells are available in the Relay desktop app. This browser view does not emulate a terminal.' : !ready ? 'Native workspace initialization has not completed.' : appInfo?.shells.length ? 'Each shell opens in its own terminal tab.' : 'No supported local shells were detected.';
  $<HTMLDialogElement>('#shell-dialog').showModal();
}
function openDefaultShell() {
  if (!desktopRequired() || !appInfo?.shells.length) { showShells(); return; }
  const shell = appInfo.shells[0]; openSession({ kind: 'local', shell: shell.id }, shell.name);
}
function openSsh(profile: SshProfile) {
  if (!desktopRequired()) return;
  if (!appInfo?.sshAvailable) { notify('An SSH client was not found on this computer.', true); return; }
  openSession({ kind: 'ssh', profileId: profile.id }, profile.name, profile);
}

function openSession(request: Omit<OpenRequest, 'id' | 'cols' | 'rows'>, title: string, profile?: SshProfile) {
  if (!desktopRequired()) return;
  let releaseProfile = () => {};
  try { if (profile) releaseProfile = profileActivity.use(profile.id); }
  catch (error) { notify(errorText(error), true); return; }
  const id = crypto.randomUUID();
  const host = el('div', 'terminal-host'); host.id = `terminal-${id}`; host.setAttribute('role', 'tabpanel'); host.setAttribute('aria-label', title); $('#terminal-hosts').append(host);
  const term = new Terminal({ cursorBlink: true, cursorStyle: 'bar', fontSize: settings.fontSize, fontFamily: '"Cascadia Code", "Cascadia Mono", "SFMono-Regular", Consolas, monospace', scrollback: settings.scrollback, allowProposedApi: false, disableStdin: true, theme: { background: '#15191f', foreground: '#d7dde5', cursor: '#eaba73', selectionBackground: '#54617188', black: '#242b35', red: '#e78383', green: '#9dc9a1', yellow: '#e8bd7c', blue: '#8badce', magenta: '#bda2d2', cyan: '#86c7c8', white: '#e0e6ec', brightBlack: '#69798d', brightRed: '#f4aaaa', brightGreen: '#c0e3bd', brightYellow: '#f3d7a2', brightBlue: '#abc6e1', brightMagenta: '#d5bcea', brightCyan: '#ace0de', brightWhite: '#ffffff' } });
  term.options.disableStdin = false;
  const fit = new FitAddon(); const search = new SearchAddon(); term.loadAddon(fit); term.loadAddon(search); term.open(host);
  let releaseInput = () => {};
  const inputReady = new Promise<void>(resolve => { releaseInput = resolve; });
  const lifetime = new SessionLifetime(() => api.close(id), error => notify(`Could not close session: ${errorText(error)}`, true));
  const session: WorkspaceSession = { id, request: { ...request, id, cols: 80, rows: 24 }, title, profile: profile ? structuredClone(profile) : undefined, term, fit, search, host, state: 'opening', detail: 'Connecting…', lifetime, releaseProfile, queue: new SerialQueue(), cols: 80, rows: 24, inputReady, releaseInput };
  sessions.set(id, session); selectSession(id);
  fitSession(session, false); session.request.cols = session.cols; session.request.rows = session.rows;
  term.onData(data => sendInput(session, data));
  term.onBinary(data => sendBytes(session, Uint8Array.from(data, char => char.charCodeAt(0) & 255)));
  term.attachCustomKeyEventHandler(event => {
    if (event.type !== 'keydown') return true;
    if (isWorkspaceShortcut(event)) { handleShortcut(event); return false; }
    if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.code === 'KeyC') { event.preventDefault(); void copySelection(); return false; }
    if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.code === 'KeyV') { event.preventDefault(); void pasteClipboard(); return false; }
    return true;
  });
  // Register the complete handle before invoking native: channel data can arrive
  // before the open-session promise resolves.
  void api.open(session.request, event => receiveEvent(session, event)).then(info => {
    session.lifetime.markOpened();
    session.releaseInput();
    if (session.lifetime.disposed) return;
    session.title = info.title || title;
    if (session.state === 'opening') { session.state = 'live'; session.detail = request.kind === 'ssh' ? `${profile?.user}@${profile?.host}:${profile?.port}` : title; term.options.disableStdin = false; }
    session.cols = 0; session.rows = 0; fitSession(session, true); renderTabs(); renderActive();
  }).catch(error => {
    session.releaseInput();
    if (session.lifetime.disposed) return;
    session.state = 'error'; session.detail = errorText(error); session.term.options.disableStdin = true; renderTabs(); renderActive();
  });
}

function sendInput(session: WorkspaceSession, data: string) {
  sendBytes(session, new TextEncoder().encode(data));
}
function sendBytes(session: WorkspaceSession, data: Uint8Array) {
  if (session.lifetime.disposed || !['live', 'opening'].includes(session.state)) return;
  void session.queue.run(async () => {
    await session.inputReady;
    for (let offset = 0; offset < data.length; offset += 32768) {
      if (session.lifetime.disposed || !['live', 'opening'].includes(session.state)) return;
      await api.writeBytes(session.id, Array.from(data.subarray(offset, offset + 32768)));
    }
  }).catch(error => { session.detail = `Input failed: ${errorText(error)}`; if (activeId === session.id) renderActive(); notify(session.detail, true); });
}
function receiveEvent(session: WorkspaceSession, event: TerminalEvent) {
  if (event.id !== session.id) return;
  if (event.type === 'data') {
    // Native registers its session before emitting data. Release CPR and other
    // terminal replies here even when open() is still waiting for ConPTY.
    session.releaseInput();
    if (session.lifetime.disposed) { void api.ack(session.id, event.seq).catch(() => undefined); return; }
    session.term.write(Uint8Array.from(event.data), () => { void api.ack(session.id, event.seq).catch(error => { if (!session.lifetime.disposed && session.state !== 'exited') notify(`Terminal flow control: ${errorText(error)}`, true); }); });
  } else if (!session.lifetime.disposed) {
    session.releaseInput();
    session.state = event.type === 'exit' ? 'exited' : 'error';
    session.detail = event.type === 'exit' ? `Process exited${event.code === null ? '' : ` with code ${event.code}`}.` : event.message;
    session.term.options.disableStdin = true; renderTabs(); renderActive();
  }
}
function selectSession(id: string) {
  if (!sessions.has(id)) return;
  const changed = activeId !== id; activeId = id;
  for (const s of sessions.values()) s.host.classList.toggle('active', s.id === id);
  $('#empty-state').classList.add('hidden'); renderTabs(); renderActive();
  requestAnimationFrame(() => { const s = active(); if (s) { fitSession(s, true); s.term.focus(); } });
  if (changed) { resetRemote(); if (filePanelOpen) void loadRemote('.'); }
}
function renderTabs() {
  const tabs = $('#tabs'); tabs.replaceChildren();
  for (const s of sessions.values()) {
    const tab = el('div', `tab ${s.id === activeId ? 'selected' : ''}`);
    const select = el('button', 'tab-select'); select.type = 'button'; select.setAttribute('role', 'tab'); select.setAttribute('aria-selected', String(s.id === activeId)); select.setAttribute('aria-controls', s.host.id); select.tabIndex = s.id === activeId ? 0 : -1;
    const dot = el('span', `tab-dot ${s.state}`); select.append(dot, el('span', 'tab-title', s.title)); select.addEventListener('click', () => selectSession(s.id));
    tab.append(select, action(`Close ${s.title}`, 'close', () => closeSession(s.id), 'tab-close icon-button')); tabs.append(tab);
  }
  $('#empty-state').classList.toggle('hidden', sessions.size > 0);
}
function renderActive() {
  const s = active(); $('#workspace-title').textContent = s ? s.profile ? s.profile.name : 'Local workspace' : 'Your next connection starts here';
  $('#session-context').textContent = s?.detail || 'No active session'; $('#session-indicator').className = `status-dot ${s?.state ?? ''}`;
  $('#session-message').classList.toggle('hidden', !s || !['error', 'exited'].includes(s.state));
  $('#session-message').textContent = s?.detail || '';
  $('#status-left').textContent = s ? `${s.request.kind === 'local' ? 'LOCAL' : 'SSH'} · ${s.state === 'opening' ? 'Connecting' : s.state === 'live' ? 'Connected' : s.detail}` : native ? ready ? 'Ready' : 'Native services unavailable' : 'Browser preview · native services unavailable';
  $('#status-right').textContent = s ? `${s.cols} × ${s.rows}  /  UTF-8  /  ${settings.fontSize}px` : 'UTF-8  /  Relay';
  const buttons = $('#terminal-actions').querySelectorAll<HTMLButtonElement>('button');
  for (const b of buttons) b.disabled = !s || (b.dataset.live === 'true' && s.state !== 'live');
  const files = $<HTMLButtonElement>('#files-toggle'); files.disabled = !s?.profile || !appInfo?.sftpAvailable; files.classList.toggle('toggled', filePanelOpen); files.setAttribute('aria-pressed', String(filePanelOpen));
}
function closeSession(id: string) {
  const s = sessions.get(id); if (!s) return;
  s.lifetime.close(); s.releaseInput(); s.releaseProfile(); sessions.delete(id); s.term.dispose(); s.host.remove();
  if (activeId === id) { activeId = null; const next = [...sessions.keys()].at(-1); if (next) selectSession(next); else { resetRemote(); renderTabs(); renderActive(); } }
  else renderTabs();
}
function reconnect() {
  const s = active(); if (!s) return;
  const { kind, shell, cwd, profileId } = s.request; const profile = s.profile; const title = s.title;
  closeSession(s.id); openSession({ kind, shell, cwd, profileId }, title, profile);
}
function fitSession(s: WorkspaceSession, nativeResize: boolean) {
  if (s.lifetime.disposed || activeId !== s.id || s.host.clientWidth < 40 || s.host.clientHeight < 40) return;
  try { s.fit.fit(); } catch { return; }
  const cols = Math.max(10, s.term.cols); const rows = Math.max(3, s.term.rows);
  const changed = cols !== s.cols || rows !== s.rows; s.cols = cols; s.rows = rows;
  if (nativeResize && s.lifetime.opened && s.state === 'live' && changed) void api.resize(s.id, cols, rows).catch(error => notify(`Resize failed: ${errorText(error)}`, true));
  renderActive();
}
let resizeTimer = 0;
const resizeObserver = new ResizeObserver(() => { window.clearTimeout(resizeTimer); resizeTimer = window.setTimeout(() => { const s = active(); if (s) fitSession(s, true); }, 80); });
resizeObserver.observe($('#terminal-area'));

async function copySelection() {
  const text = active()?.term.getSelection(); if (!text) { notify('Select terminal text to copy.'); return; }
  try { await navigator.clipboard.writeText(text); notify('Selection copied.'); } catch (error) { notify(`Clipboard access failed: ${errorText(error)}`, true); }
}
async function pasteClipboard() {
  const s = active(); if (!s || s.state !== 'live') return;
  try { const text = await navigator.clipboard.readText(); if (sessions.get(s.id) === s && !s.lifetime.disposed && s.state === 'live') { s.term.paste(text); s.term.focus(); } }
  catch (error) { notify(`Clipboard access failed: ${errorText(error)}`, true); }
}
function showSearch() { if (!active()) return; $('#terminal-search').classList.remove('hidden'); $<HTMLInputElement>('#terminal-search-input').focus(); }
function runSearch(previous = false) {
  const term = active(); const value = $<HTMLInputElement>('#terminal-search-input').value;
  if (!term || !value) { $('#search-result').textContent = ''; return; }
  const found = previous ? term.search.findPrevious(value) : term.search.findNext(value);
  $('#search-result').textContent = found ? '' : 'No match';
}
function closeSearch() { $('#terminal-search').classList.add('hidden'); active()?.search.clearDecorations(); active()?.term.focus(); }
function changeFont(delta: number) {
  if (!native) return;
  void persist(current => ({ ...current, fontSize: Math.max(10, Math.min(28, current.fontSize + delta)) })).then(() => {
    for (const s of sessions.values()) s.term.options.fontSize = settings.fontSize;
    const s = active(); if (s) fitSession(s, true);
  }).catch(() => undefined);
}
const toolbar = $('#terminal-actions');
toolbar.append(action('Reconnect session', 'refresh', reconnect), el('span', 'toolbar-divider'), action('Find in terminal (Ctrl+Shift+F)', 'search', showSearch), action('Copy selection (Ctrl+Shift+C)', 'copy', () => void copySelection()));
const paste = action('Paste (Ctrl+Shift+V)', 'paste', () => void pasteClipboard()); paste.dataset.live = 'true'; toolbar.append(paste, el('span', 'toolbar-divider'));
for (const [label, text, delta] of [['Decrease font size', 'A−', -1], ['Increase font size', 'A+', 1]] as const) { const b = el('button', 'icon-button font-button', text); b.title = label; b.setAttribute('aria-label', label); b.addEventListener('click', () => changeFont(delta)); toolbar.append(b); }
toolbar.append(el('span', 'toolbar-divider')); const fileToggle = action('Toggle remote files', 'folder', () => toggleFiles()); fileToggle.id = 'files-toggle'; toolbar.append(fileToggle);

function toggleFiles(force?: boolean) {
  filePanelOpen = force ?? !filePanelOpen; $('#file-panel').classList.toggle('hidden', !filePanelOpen); renderActive();
  if (filePanelOpen) void loadRemote(remoteProfileId === active()?.profile?.id ? remotePath : '.');
}
function resetRemote() { remoteGeneration++; remoteProfileId = null; remoteEntries = []; remotePath = '.'; remoteBusy = false; $('#remote-files').replaceChildren(); $('#remote-status').textContent = 'Select an SSH session to browse its files.'; $<HTMLInputElement>('#remote-path').value = ''; updateRemoteControls(); }
function updateRemoteControls() {
  const allowed = !!active()?.profile && !!appInfo?.sftpAvailable;
  for (const b of $('#file-panel').querySelectorAll<HTMLButtonElement>('.file-actions button, .remote-path button')) b.disabled = !allowed || remoteBusy;
  $<HTMLInputElement>('#remote-path').disabled = !allowed || remoteBusy;
  $<HTMLButtonElement>('#remote-upload').disabled = !allowed || remoteBusy || !remoteProfileId;
}
async function loadRemote(path: string) {
  const profile = active()?.profile; if (!profile || !appInfo?.sftpAvailable) { resetRemote(); return; }
  const generation = ++remoteGeneration; remoteBusy = true; remoteProfileId = null; remoteEntries = []; updateRemoteControls(); $('#remote-status').textContent = 'Reading remote directory…'; $('#remote-status').classList.remove('is-error'); $('#remote-files').replaceChildren();
  let releaseProfile = () => {};
  try {
    releaseProfile = profileActivity.use(profile.id);
    const listing = await api.listRemote(profile.id, path);
    if (generation !== remoteGeneration || active()?.profile?.id !== profile.id) return;
    remotePath = listing.path; remoteProfileId = profile.id; remoteEntries = listing.entries;
    $<HTMLInputElement>('#remote-path').value = listing.path;
    $('#remote-status').textContent = `${listing.entries.length} ${listing.entries.length === 1 ? 'item' : 'items'}`;
    for (const entry of [...listing.entries].sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name))) {
      const row = el('div', 'remote-entry'); const name = el('button', 'remote-entry-name'); name.type = 'button'; name.innerHTML = svg(entry.isDir ? 'folder' : 'file'); name.append(el('span', '', entry.name)); name.title = entry.path;
      name.addEventListener('click', () => entry.isDir ? void loadRemote(entry.path) : void downloadEntry(profile.id, entry));
      row.append(name, el('span', 'remote-entry-size', entry.isDir ? '' : formatBytes(entry.size)));
      if (!entry.isDir) row.append(action(`Download ${entry.name}`, 'download', () => void downloadEntry(profile.id, entry))); $('#remote-files').append(row);
    }
    if (!listing.entries.length) $('#remote-files').append(el('p', 'files-empty', 'This directory is empty.'));
  } catch (error) { if (generation === remoteGeneration) { $('#remote-status').textContent = `Could not read ${path}: ${errorText(error)}`; $('#remote-status').classList.add('is-error'); $<HTMLInputElement>('#remote-path').value = remotePath; } }
  finally { releaseProfile(); if (generation === remoteGeneration) { remoteBusy = false; updateRemoteControls(); } }
}
async function downloadEntry(profileId: string, entry: RemoteEntry) {
  let releaseProfile = () => {};
  try { releaseProfile = profileActivity.use(profileId); const path = await pickSave({ title: `Download ${entry.name}`, defaultPath: entry.name }); if (!path) return; notify(`Downloading ${entry.name}…`); const result = await api.download(profileId, entry.path, path); notify(`Downloaded ${formatBytes(result.bytes)} to ${result.localPath}`); }
  catch (error) { notify(`Download failed: ${errorText(error)}`, true); }
  finally { releaseProfile(); }
}
async function uploadFile() {
  const profileId = remoteProfileId; const directory = remotePath;
  if (!profileId || remoteBusy) return;
  let releaseProfile = () => {};
  try {
    releaseProfile = profileActivity.use(profileId);
    const path = await pickOpen({ title: 'Choose a file to upload', directory: false, multiple: false }); if (typeof path !== 'string') return;
    if (remoteProfileId !== profileId || remotePath !== directory) throw new Error('The remote folder changed. Choose Upload again in the intended folder.');
    const name = path.split(/[\\/]/).at(-1)!; const remote = remoteJoin(directory, name);
    if (remoteEntries.some(e => e.name === name)) throw new Error(`${name} already exists in ${directory}. Rename the local file or choose another folder; existing files are never replaced.`);
    notify(`Uploading ${name}…`); const result = await api.upload(profileId, path, remote); notify(`Uploaded ${formatBytes(result.bytes)} to ${result.remotePath}`);
    if (remoteProfileId === profileId && remotePath === directory) await loadRemote(directory);
  } catch (error) { notify(`Upload failed: ${errorText(error)}`, true); }
  finally { releaseProfile(); }
}

function renderBookmarks() {
  const list = $('#bookmark-list'); list.replaceChildren(); if (!bookmarks) return;
  const query = $<HTMLInputElement>('#bookmark-filter').value.toLowerCase();
  const entries = bookmarks.entries.filter(e => `${e.name} ${e.folder} ${e.section}`.toLowerCase().includes(query));
  for (const entry of entries.slice(0, 250)) {
    const b = el('button', 'bookmark-entry'); b.append(el('strong', '', entry.name), el('span', '', `${entry.folder || 'Root'} · ${entry.section}${entry.ignoredBySource ? ' · skipped after section gap' : ''}`));
    b.addEventListener('click', () => { $<HTMLTextAreaElement>('#bookmark-value').value = entry.rawValue; for (const x of list.children) x.classList.remove('selected'); b.classList.add('selected'); }); list.append(b);
  }
  if (!entries.length) list.append(el('p', 'files-empty', 'No matching bookmarks.'));
  if (entries.length > 250) list.append(el('p', 'form-note', `Showing the first 250 of ${entries.length} matches. Refine the filter to find a specific bookmark.`));
}
$('#pick-bookmarks').addEventListener('click', async () => {
  if (!desktopRequired()) return;
  const generation = ++bookmarkGeneration;
  const ownsDialog = () => generation === bookmarkGeneration && $<HTMLDialogElement>('#bookmark-dialog').open;
  try {
    const path = await pickOpen({ title: 'Inspect MobaXterm bookmarks', directory: false, multiple: false, filters: [{ name: 'MobaXterm settings', extensions: ['ini', 'mxtsessions'] }] }); if (typeof path !== 'string') return;
    if (!ownsDialog()) return;
    bookmarks = null; $('#bookmark-list').replaceChildren(); $('#bookmark-warnings').replaceChildren(); $<HTMLTextAreaElement>('#bookmark-value').value = '';
    $('#bookmark-source').textContent = 'Reading bookmark inventory…'; const inventory = await api.inspectBookmarks(path);
    if (!ownsDialog()) return;
    bookmarks = inventory;
    $('#bookmark-source').textContent = `${bookmarks.entries.length} bookmarks · ${bookmarks.source}`;
    $('#bookmark-warnings').replaceChildren(...bookmarks.warnings.map(w => el('p', '', w))); $<HTMLTextAreaElement>('#bookmark-value').value = ''; renderBookmarks();
  } catch (error) { if (ownsDialog()) $('#bookmark-source').textContent = `Could not inspect file: ${errorText(error)}`; }
});

function isWorkspaceShortcut(event: KeyboardEvent): boolean {
  const mod = event.ctrlKey || event.metaKey;
  return mod && ((event.shiftKey && ['KeyT', 'KeyW', 'KeyF'].includes(event.code)) || event.key === 'Tab' || ['Equal', 'Minus', 'NumpadAdd', 'NumpadSubtract'].includes(event.code));
}
function handleShortcut(event: KeyboardEvent) {
  if (event.defaultPrevented || !isWorkspaceShortcut(event) || document.querySelector('dialog[open]')) return;
  event.preventDefault();
  if (event.code === 'KeyT') openDefaultShell();
  else if (event.code === 'KeyW') { if (activeId) closeSession(activeId); }
  else if (event.code === 'KeyF') showSearch();
  else if (event.key === 'Tab') { const ids = [...sessions.keys()]; if (ids.length) { const index = ids.indexOf(activeId!); selectSession(ids[(index + (event.shiftKey ? ids.length - 1 : 1)) % ids.length]); } }
  else changeFont(['Minus', 'NumpadSubtract'].includes(event.code) ? -1 : 1);
}
document.addEventListener('keydown', handleShortcut);
$('#new-session').addEventListener('click', () => showConnection()); $('#empty-ssh').addEventListener('click', () => showConnection());
$('#shell-button').addEventListener('click', showShells); $('#empty-shell').addEventListener('click', openDefaultShell); $('#add-tab').addEventListener('click', openDefaultShell);
$('#profile-filter').addEventListener('input', renderProfiles); $('#bookmark-filter').addEventListener('input', renderBookmarks);
$('#terminal-search-input').addEventListener('input', () => runSearch()); $('#terminal-search-input').addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); runSearch(event.shiftKey); } if (event.key === 'Escape') closeSearch(); });
$('#search-next').addEventListener('click', () => runSearch()); $('#search-previous').addEventListener('click', () => runSearch(true)); $('#search-close').addEventListener('click', closeSearch);
$('#files-close').addEventListener('click', () => toggleFiles(false)); $('#remote-up').addEventListener('click', () => void loadRemote(remoteParent(remotePath))); $('#remote-refresh').addEventListener('click', () => void loadRemote(remotePath)); $('#remote-upload').addEventListener('click', () => void uploadFile());
$('#remote-path-form').addEventListener('submit', event => { event.preventDefault(); const path = $<HTMLInputElement>('#remote-path').value.trim(); if (path) void loadRemote(path); });
$('#inspect-bookmarks').addEventListener('click', () => { bookmarkGeneration++; $<HTMLDialogElement>('#bookmark-dialog').showModal(); });
$<HTMLDialogElement>('#bookmark-dialog').addEventListener('close', () => { bookmarkGeneration++; });
for (const [button, dialog] of [['connection-close', 'connection-dialog'], ['shell-close', 'shell-dialog'], ['bookmarks-close', 'bookmark-dialog']]) $(`#${button}`).addEventListener('click', () => $<HTMLDialogElement>(`#${dialog}`).close());
for (const dialog of document.querySelectorAll('dialog')) dialog.addEventListener('close', () => active()?.term.focus());
window.addEventListener('beforeunload', () => { resizeObserver.disconnect(); for (const s of sessions.values()) { s.lifetime.close(); s.releaseInput(); s.releaseProfile(); s.term.dispose(); } });

async function initialize() {
  renderProfiles(); renderActive(); resetRemote();
  if (!native) {
    $('#platform-label').textContent = 'Browser preview'; $('#version-label').textContent = 'Native services are not connected'; $('#native-badge').textContent = 'PREVIEW';
    $('#empty-note').textContent = 'You’re viewing Relay in a browser. Open the desktop app to start a real shell or SSH session.'; return;
  }
  try {
    const [infoResult, savedResult] = await Promise.allSettled([api.appInfo(), api.loadSettings()]);
    if (infoResult.status === 'rejected') throw infoResult.reason;
    const info = infoResult.value; appInfo = info; ready = true;
    if (savedResult.status === 'fulfilled') { settings = savedResult.value; settingsReady = true; }
    else notify(`Settings could not be loaded; saving is disabled: ${errorText(savedResult.reason)}`, true);
    $('#platform-label').textContent = `${info.platform} · desktop`; $('#version-label').textContent = settingsReady ? `Relay ${info.version} · local settings` : 'Settings unavailable · saving disabled'; $('#native-badge').textContent = settingsReady ? 'LOCAL FIRST' : 'SETTINGS ERROR';
    $('#empty-note').textContent = info.sshAvailable ? 'Connect with SSH keys, an agent, or an interactive sign-in.' : 'Local shells are ready. Install an SSH client to connect remotely.';
    renderProfiles(); renderActive();
    if (info.shells.length) openDefaultShell();
  } catch (error) { $('#platform-label').textContent = 'Initialization failed'; $('#version-label').textContent = 'Restart Relay to retry'; $('#native-badge').textContent = 'UNAVAILABLE'; $('#empty-note').textContent = errorText(error); notify(`Could not initialize workspace: ${errorText(error)}`, true); renderActive(); }
}
void initialize();
