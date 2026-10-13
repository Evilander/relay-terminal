import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { createServer } from 'vite';
import { chromium, expect } from '@playwright/test';

const output = path.resolve(process.argv[2] ?? `.artifacts/connection-ui-${Date.now()}`);
await mkdir(path.dirname(output), { recursive: true });
await mkdir(output, { recursive: false });
const proof = { mode: 'browser UI with mocked Tauri IPC and file pickers', checks: [] };
let server;
let browser;
let page;
let cleaning;
const cleanup = () => cleaning ??= (async () => {
  try { if (browser) await browser.close(); }
  finally { if (server) await server.close(); }
})();
const deadline = setTimeout(() => {
  console.error('Connection UI smoke exceeded its three-minute deadline');
  const exit = setTimeout(() => process.exit(1), 10000);
  void cleanup().finally(() => { clearTimeout(exit); process.exit(1); });
}, 180000);
try {
  server = await createServer({ root: process.cwd(), server: { host: '127.0.0.1', port: 0 }, logLevel: 'error' });
  await server.listen();
  const port = server.httpServer.address().port;
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  page = await browser.newPage({ viewport: { width: 1320, height: 860 } });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    const existing = { id: 'existing-id', name: 'Existing connection', folder: 'Work', host: 'old.example.invalid', port: 22, user: 'alice', keyPath: null, jumpHost: null };
    const fresh = { ...existing, id: 'foreign-id', name: '<img src=x onerror="window.pwned=true">', host: 'new.example.invalid' };
    const state = window.__RELAY_UI_TEST__ = {
      settings: { profiles: [existing], fontSize: 19, scrollback: 12345 },
      incoming: { format: 'relay.connections', version: 1, profiles: [existing, fresh, { ...fresh, id: 'foreign-repeat', name: 'Repeated endpoint' }] },
      open: [], save: [], backups: {}, holdSave: false, holdPicker: false, saveCalls: 0,
    };
    window.isTauri = true;
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'app_info') return { version: 'test', platform: 'windows', home: 'C:\\test', shells: [], sshAvailable: false, sftpAvailable: false };
        if (command === 'load_settings') return structuredClone(state.settings);
        if (command === 'save_settings') {
          if (state.holdSave) await new Promise(resolve => { state.releaseSave = resolve; });
          state.settings = structuredClone(args.settings); state.saveCalls++; return;
        }
        if (command === 'plugin:dialog|open') {
          if (state.holdPicker) { state.holdPicker = false; return new Promise(resolve => { state.releasePicker = resolve; }); }
          return state.open.shift() ?? null;
        }
        if (command === 'plugin:dialog|save') return state.save.shift() ?? null;
        if (command === 'inspect_connection_file') return structuredClone(state.incoming);
        if (command === 'export_connections') {
          if (Object.hasOwn(state.backups, args.path)) throw new Error('The destination already exists. Choose a new filename.');
          state.backups[args.path] = JSON.stringify({ format: 'relay.connections', version: 1, profiles: state.settings.profiles });
          return state.settings.profiles.length;
        }
        throw new Error(`Unexpected mocked IPC command: ${command}`);
      },
    };
  });
  await page.goto(`http://127.0.0.1:${port}`, { waitUntil: 'commit' });
  await expect(page.locator('#native-badge')).toHaveText('LOCAL FIRST', { timeout: 30000 });
  const snapshot = () => page.evaluate(() => structuredClone(window.__RELAY_UI_TEST__.settings));
  const initial = await snapshot();
  const openImport = async () => {
    await page.evaluate(() => window.__RELAY_UI_TEST__.open.push('/mock/incoming.json'));
    await page.locator('#import-connections').click();
    await expect(page.locator('#import-preview-dialog')).toBeVisible();
  };
  await openImport();
  await expect(page.locator('#import-summary')).toHaveText('1 new connections · 2 duplicates skipped');
  await expect(page.locator('#import-list img')).toHaveCount(0);
  assert.equal(await page.evaluate(() => window.pwned), undefined);
  assert.deepEqual(await snapshot(), initial);
  await page.locator('#import-cancel').click();
  assert.deepEqual(await snapshot(), initial);
  proof.checks.push('Real import preview and cancellation preserve settings; untrusted names render as text');
  await openImport();
  await page.locator('#confirm-import').click();
  await expect(page.locator('#import-preview-dialog')).not.toBeVisible();
  const merged = await snapshot();
  assert.equal(merged.profiles.length, 2);
  assert.deepEqual(merged.profiles[0], initial.profiles[0]);
  assert.notEqual(merged.profiles[1].id, 'foreign-id');
  assert.equal(merged.fontSize, initial.fontSize); assert.equal(merged.scrollback, initial.scrollback);
  proof.checks.push('Confirmation merges one endpoint, regenerates its ID, and preserves profiles and appearance settings');
  await openImport();
  await expect(page.locator('#import-summary')).toHaveText('0 new connections · 3 duplicates skipped');
  await expect(page.locator('#confirm-import')).toBeDisabled();
  await page.locator('#import-cancel').click();
  assert.deepEqual(await snapshot(), merged);
  proof.checks.push('Reimport previews duplicates and disables confirmation');
  await page.evaluate(() => window.__RELAY_UI_TEST__.save.push('/mock/backup.json'));
  await page.locator('#export-connections').click();
  await expect(page.locator('#toast')).toContainText('Exported 2 connections');
  const exported = await page.evaluate(() => window.__RELAY_UI_TEST__.backups['/mock/backup.json']);
  assert.deepEqual(JSON.parse(exported).profiles, merged.profiles);
  assert.deepEqual(Object.keys(JSON.parse(exported)).sort(), ['format', 'profiles', 'version']);
  await page.evaluate(() => window.__RELAY_UI_TEST__.save.push('/mock/backup.json'));
  await page.locator('#export-connections').click();
  await expect(page.locator('#toast')).toContainText('already exists');
  assert.equal(await page.evaluate(() => window.__RELAY_UI_TEST__.backups['/mock/backup.json']), exported);
  proof.checks.push('Export UI uses profiles-only payload and reports mocked collision without changing its prior backup');
  await page.locator('#new-session').click();
  await page.evaluate(() => { window.__RELAY_UI_TEST__.holdPicker = true; });
  await page.locator('#pick-key').click();
  await page.keyboard.press('Escape');
  await expect(page.locator('#connection-dialog')).not.toBeVisible();
  await page.locator('#new-session').click();
  await page.evaluate(() => window.__RELAY_UI_TEST__.releasePicker('C:\\mock\\stale-key'));
  await expect(page.locator('#connection-form [name="keyPath"]')).toHaveValue('');
  proof.checks.push('A delayed mocked key picker cannot change a reopened connection dialog');
  await page.locator('#connection-form [name="name"]').fill('Saved through real form');
  await page.locator('#connection-form [name="host"]').fill('third.example.invalid');
  await page.locator('#connection-form [name="user"]').fill('alice');
  await page.evaluate(() => { window.__RELAY_UI_TEST__.holdSave = true; });
  await page.locator('#connection-form button[value="save"]').click();
  await expect(page.locator('#connection-form [name="name"]')).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(page.locator('#connection-dialog')).toBeVisible();
  await page.evaluate(() => { window.__RELAY_UI_TEST__.holdSave = false; window.__RELAY_UI_TEST__.releaseSave(); });
  await expect(page.locator('#connection-dialog')).not.toBeVisible();
  assert.equal((await snapshot()).profiles.length, 3);
  proof.checks.push('Connection save disables editing and Escape until the mocked persistence operation completes');
  assert.deepEqual(errors, []);
  proof.checks.push('No unhandled frontend errors observed');
  proof.passed = true;
  await page.screenshot({ path: path.join(output, 'connection-ui.png') });
} catch (error) {
  proof.passed = false; proof.error = String(error?.stack ?? error); process.exitCode = 1;
  if (page) await page.screenshot({ path: path.join(output, 'failure.png') }).catch(() => {});
} finally {
  try { await writeFile(path.join(output, 'proof.json'), JSON.stringify(proof, null, 2)); }
  finally { try { await cleanup(); } finally { clearTimeout(deadline); } }
}
console.log(JSON.stringify({ output, ...proof }));
