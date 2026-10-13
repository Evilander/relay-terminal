import type { Settings, SshProfile } from './api';

export class SerialQueue {
  private tail: Promise<unknown> = Promise.resolve();

  run<T>(operation: () => Promise<T>): Promise<T> {
    const task = this.tail.then(operation);
    this.tail = task.catch(() => undefined);
    return task;
  }
}

export class SessionLifetime {
  opened = false;
  disposed = false;

  constructor(private closeNative: () => Promise<void>, private reportError: (error: unknown) => void) {}

  markOpened() {
    this.opened = true;
    // Closing before native registered the session can be a no-op. Retry once
    // its open result arrives, while also cancelling a registered startup now.
    if (this.disposed) this.requestClose();
  }

  close() {
    if (this.disposed) return;
    this.disposed = true;
    this.requestClose();
  }

  private requestClose() { void this.closeNative().catch(this.reportError); }
}

export class ProfileActivity {
  private users = new Map<string, number>();
  private mutations = new Set<string>();

  isInUse(id: string): boolean { return (this.users.get(id) ?? 0) > 0 || this.mutations.has(id); }

  use(id: string): () => void {
    if (this.mutations.has(id)) throw new Error('This profile is being saved. Try again when saving finishes.');
    this.users.set(id, (this.users.get(id) ?? 0) + 1);
    return this.once(() => {
      const remaining = this.users.get(id)! - 1;
      if (remaining) this.users.set(id, remaining); else this.users.delete(id);
    });
  }

  mutate(id: string): () => void {
    if (this.isInUse(id)) throw new Error('Close every tab for this profile and wait for its file operations to finish before editing or deleting it.');
    this.mutations.add(id);
    return this.once(() => { this.mutations.delete(id); });
  }

  private once(release: () => void): () => void {
    let released = false;
    return () => { if (!released) { released = true; release(); } };
  }
}

export function remoteParent(path: string): string {
  const normalized = path.replace(/\/+$/, '');
  if (!normalized && path.startsWith('/')) return '/';
  const split = normalized.lastIndexOf('/');
  return split < 0 ? '.' : normalized.slice(0, split) || '/';
}

export function remoteJoin(path: string, filename: string): string {
  if (!filename || /[\x00/\\]/.test(filename) || filename === '.' || filename === '..') {
    throw new Error('Choose a file with a regular filename.');
  }
  return `${path.replace(/\/+$/, '')}/${filename}`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function previewConnectionImport(saved: readonly SshProfile[], incoming: readonly SshProfile[]): { additions: SshProfile[]; duplicates: number } {
  const endpoint = (p: SshProfile) => JSON.stringify([p.user, p.host, p.port, p.keyPath, p.jumpHost]);
  const seen = new Set(saved.map(endpoint));
  const additions: SshProfile[] = [];
  for (const profile of incoming) {
    const key = endpoint(profile);
    if (seen.has(key)) continue;
    seen.add(key); additions.push(profile);
  }
  return { additions, duplicates: incoming.length - additions.length };
}

export function mergeConnectionImport(current: Settings, incoming: readonly SshProfile[], makeId: () => string = () => crypto.randomUUID()): { settings: Settings; added: number; duplicates: number } {
  const preview = previewConnectionImport(current.profiles, incoming);
  if (current.profiles.length + preview.additions.length > 1000) throw new Error('Import would exceed the limit of 1,000 saved connections. Remove some profiles or choose a smaller file.');
  const usedIds = new Set(current.profiles.map(p => p.id));
  const additions = preview.additions.map(profile => {
    let id = makeId();
    for (let attempt = 0; usedIds.has(id) && attempt < 10; attempt++) id = makeId();
    if (usedIds.has(id)) throw new Error('Could not assign a unique connection ID. Try importing again.');
    usedIds.add(id); return { ...profile, id };
  });
  return { settings: { ...current, profiles: [...current.profiles, ...additions] }, added: additions.length, duplicates: preview.duplicates };
}
