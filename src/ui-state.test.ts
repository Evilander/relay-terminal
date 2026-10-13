import { describe, expect, it, vi } from 'vitest';
import { SerialQueue, SessionLifetime, ProfileActivity, remoteJoin, remoteParent, previewConnectionImport, mergeConnectionImport } from './ui-state';
import type { Settings, SshProfile } from './api';

describe('native session lifetime', () => {
  it('cancels native startup immediately without waiting for its open result', async () => {
    const close = vi.fn(async () => {});
    const lifetime = new SessionLifetime(close, vi.fn());
    let resolveOpen!: () => void;
    const pendingOpen = new Promise<void>(resolve => { resolveOpen = resolve; }).then(() => lifetime.markOpened());
    lifetime.close();
    expect(close).toHaveBeenCalledTimes(1);
    expect(lifetime.disposed).toBe(true);
    expect(lifetime.opened).toBe(false);
    // If the first close preceded native registration, the late result must
    // close again rather than leave an unowned process behind.
    resolveOpen(); await pendingOpen;
    expect(close).toHaveBeenCalledTimes(2);
  });
  it('closes a live session once when teardown is repeated', () => {
    const close = vi.fn(async () => {});
    const lifetime = new SessionLifetime(close, vi.fn());
    lifetime.markOpened(); lifetime.close(); lifetime.close();
    expect(close).toHaveBeenCalledTimes(1);
  });
  it('reports close failure while preserving the late-open cleanup', async () => {
    const error = new Error('close failed');
    const close = vi.fn().mockRejectedValueOnce(error).mockResolvedValue(undefined);
    const report = vi.fn(); const lifetime = new SessionLifetime(close, report);
    lifetime.close(); await Promise.resolve();
    expect(report).toHaveBeenCalledWith(error);
    lifetime.markOpened();
    expect(close).toHaveBeenCalledTimes(2);
  });
});

describe('SSH profile ownership', () => {
  it('retains a tab reference independently of terminal process state', () => {
    const activity = new ProfileActivity();
    const closeTab = activity.use('host-a');
    // Exit/error changes terminal state, but only closing its tab releases
    // the profile snapshot and the file browser that still belongs to it.
    expect(() => activity.mutate('host-a')).toThrow('Close every tab');
    closeTab(); const finishSave = activity.mutate('host-a'); finishSave();
    expect(activity.isInUse('host-a')).toBe(false);
  });
  it('retains the profile through a pending picker or transfer after its tab closes', () => {
    const activity = new ProfileActivity();
    const closeTab = activity.use('host-a');
    const finishTransfer = activity.use('host-a');
    closeTab(); closeTab();
    expect(() => activity.mutate('host-a')).toThrow('file operations');
    finishTransfer(); finishTransfer();
    expect(activity.isInUse('host-a')).toBe(false);
    const finishSave = activity.mutate('host-a'); finishSave();
  });
  it('prevents sessions and file operations starting during a save', () => {
    const activity = new ProfileActivity(); const finishSave = activity.mutate('host-a');
    expect(() => activity.use('host-a')).toThrow('being saved');
    expect(() => activity.mutate('host-a')).toThrow();
    const finishOther = activity.use('host-b'); finishOther();
    finishSave(); const finishUse = activity.use('host-a'); finishUse();
  });
});

describe('serialized native operations', () => {
  it('keeps later terminal input behind an unfinished write', async () => {
    const queue = new SerialQueue(); const events: string[] = [];
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    const first = queue.run(async () => { events.push('first:start'); await gate; events.push('first:end'); });
    const second = queue.run(async () => { events.push('second'); });
    await Promise.resolve(); expect(events).toEqual(['first:start']);
    release(); await Promise.all([first, second]); expect(events).toEqual(['first:start', 'first:end', 'second']);
  });
  it('reports a failed operation without poisoning later operations', async () => {
    const queue = new SerialQueue();
    const first = queue.run(async () => { throw new Error('write failed'); });
    const second = queue.run(async () => 'next');
    await expect(first).rejects.toThrow('write failed'); await expect(second).resolves.toBe('next');
  });
  it('applies settings changes against the latest committed state', async () => {
    const queue = new SerialQueue(); let settings = { size: 14, profiles: [] as string[] };
    await Promise.all([queue.run(async () => { settings = { ...settings, profiles: ['host'] }; }), queue.run(async () => { settings = { ...settings, size: settings.size + 1 }; })]);
    expect(settings).toEqual({ size: 15, profiles: ['host'] });
  });
});
describe('remote path navigation', () => {
  it('keeps root navigation at root and joins root without double slashes', () => {
    expect(remoteParent('/')).toBe('/'); expect(remoteParent('/home/user/')).toBe('/home'); expect(remoteJoin('/', 'my file')).toBe('/my file');
  });
  it('rejects local filenames that could change the selected remote directory', () => {
    for (const value of ['../secret', '..', '.', '/etc/passwd', 'bad\\name', 'nul\0name']) expect(() => remoteJoin('/home/user', value)).toThrow();
  });
});

describe('connection file import', () => {
  const profile = (id: string, host = 'example.org'): SshProfile => ({ id, name: `Connection ${id}`, folder: 'Work', host, port: 22, user: 'alice', keyPath: null, jumpHost: null });
  const settings = (profiles: SshProfile[]): Settings => ({ profiles, fontSize: 18, scrollback: 23456 });

  it('skips saved endpoints and repeats inside a file regardless of profile labels or IDs', () => {
    const saved = profile('saved');
    const renamed = { ...saved, id: 'incoming', name: 'Renamed', folder: 'Different' };
    const fresh = profile('new', 'other.example.org');
    const preview = previewConnectionImport([saved], [renamed, fresh, { ...fresh, id: 'repeat' }]);
    expect(preview.additions).toEqual([fresh]); expect(preview.duplicates).toBe(2);
  });

  it('distinguishes authentication, port, jump host and remote username', () => {
    const saved = profile('saved');
    const incoming = [
      { ...saved, id: 'port', port: 2222 }, { ...saved, id: 'user', user: 'bob' },
      { ...saved, id: 'key', keyPath: 'C:\\Keys\\other' }, { ...saved, id: 'jump', jumpHost: 'jump.example.org' },
    ];
    expect(previewConnectionImport([saved], incoming).additions).toEqual(incoming);
  });

  it('assigns new IDs while preserving existing profiles and appearance settings', () => {
    const current = settings([profile('saved')]);
    const before = structuredClone(current); const incoming = [profile('foreign-id', 'new.example.org')];
    const merged = mergeConnectionImport(current, incoming, () => 'new-local-id');
    expect(merged.settings).toEqual({ ...before, profiles: [...before.profiles, { ...incoming[0], id: 'new-local-id' }] });
    expect(merged.added).toBe(1); expect(merged.duplicates).toBe(0);
    expect(current).toEqual(before); expect(incoming[0].id).toBe('foreign-id');
  });

  it('recomputes duplicates against the latest queued settings before saving', async () => {
    const queue = new SerialQueue(); let current = settings([]);
    const incoming = profile('foreign', 'new.example.org');
    expect(previewConnectionImport(current.profiles, [incoming]).additions).toHaveLength(1);
    const save = queue.run(async () => { current = settings([profile('already-saved', incoming.host)]); });
    const imported = queue.run(async () => { const merge = mergeConnectionImport(current, [incoming], () => 'unused'); current = merge.settings; return merge; });
    await save; const merged = await imported;
    expect(merged.added).toBe(0); expect(merged.duplicates).toBe(1);
    expect(current.profiles.map(p => p.id)).toEqual(['already-saved']);
  });

  it('allows duplicates at capacity but rejects additions without modifying current settings', () => {
    const current = settings(Array.from({ length: 1000 }, (_, i) => profile(String(i), `server-${i}.example.org`)));
    const before = structuredClone(current);
    expect(mergeConnectionImport(current, [current.profiles[0]], () => 'unused').added).toBe(0);
    expect(() => mergeConnectionImport(current, [profile('overflow', 'fresh.example.org')])).toThrow('1,000');
    expect(current).toEqual(before);
  });

  it('avoids collisions with current and newly generated IDs, failing after bounded attempts', () => {
    const current = settings([profile('taken')]);
    const ids = ['taken', 'first', 'first', 'second'];
    const incoming = [profile('incoming-1', 'a.example.org'), profile('incoming-2', 'b.example.org')];
    const merged = mergeConnectionImport(current, incoming, () => ids.shift()!);
    expect(merged.settings.profiles.map(p => p.id)).toEqual(['taken', 'first', 'second']);
    expect(() => mergeConnectionImport(current, incoming, () => 'taken')).toThrow('unique connection ID');
    expect(current.profiles).toHaveLength(1);
  });
});
