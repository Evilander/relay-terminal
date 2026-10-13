import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import path from 'node:path';
import net from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, expect } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { stopOwnedChild } from './test-rust.mjs';

export async function finishSmoke(writeArtifacts, disconnect, stop) {
  try { await writeArtifacts(); }
  finally {
    try { await stop(); }
    finally {
      let timer;
      try {
        await Promise.race([disconnect(), new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error('Browser cleanup exceeded ten seconds')), 10000);
        })]);
      } finally { clearTimeout(timer); }
    }
  }
}

export async function runNativeSmoke(args) {
const [executable, fixtureFile, outputDirectory] = args.map(value => path.resolve(value));
assert(executable && fixtureFile && outputDirectory, 'Pass the executable, fixture.json, and a fresh output directory');
const fixture = JSON.parse(await readFile(fixtureFile, 'utf8'));
await mkdir(outputDirectory, { recursive: false });
const configDirectory = path.join(outputDirectory, 'config');
await mkdir(configDirectory);
await writeFile(path.join(configDirectory, 'settings.json'), JSON.stringify({ profiles: [fixture.profile], fontSize: 14, scrollback: 10000 }));
const reservation = net.createServer();
await new Promise((resolve, reject) => { reservation.once('error', reject); reservation.listen(0, '127.0.0.1', resolve); });
const port = reservation.address().port;
await new Promise(resolve => reservation.close(resolve));
const app = spawn(executable, [], {
  windowsHide: true,
  detached: process.platform !== 'win32',
  env: {
    ...process.env,
    RELAY_CONFIG_DIR: configDirectory,
    RELAY_SSH_CONFIG: fixture.sshConfig,
    RELAY_DEBUG_PORT: String(port),
    WEBVIEW2_USER_DATA_FOLDER: path.join(outputDirectory, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --remote-debugging-address=127.0.0.1`,
  },
  stdio: ['ignore', 'pipe', 'pipe'],
});
let logs = '';
let spawnError;
app.on('error', error => { spawnError = error; });
app.stdout.on('data', data => { logs = (logs + data).slice(-65536); });
app.stderr.on('data', data => { logs = (logs + data).slice(-65536); });
let browser;
let page;
const proof = { executable, checks: [] };
const errors = [];
function alive(pid) {
  try { process.kill(pid, 0); return true; } catch (error) { return error.code !== 'ESRCH'; }
}
let stopping;
function stopApp() {
  stopping ??= stopOwnedChild(app, process.platform !== 'win32');
  return stopping;
}
const deadline = setTimeout(() => {
  proof.passed = false;
  proof.error = 'Native smoke exceeded its three-minute deadline';
  console.error(proof.error);
  void stopApp().catch(error => console.error(error)).finally(() => process.exit(1));
}, 180000);
async function screen() {
  return page.locator('.terminal-host.active .xterm-rows').innerText();
}
async function command(value, expected) {
  await page.locator('.terminal-host.active .xterm-helper-textarea').focus();
  await page.keyboard.type(value);
  await page.keyboard.press('Enter');
  const output = expect.poll(screen, { timeout: 20000 });
  if (expected instanceof RegExp) await output.toMatch(expected);
  else await output.toContain(expected);
}

try {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (spawnError) throw spawnError;
    if (app.exitCode !== null) throw new Error(`App exited during startup: ${logs}`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(500) });
      if (response.ok) break;
    } catch { /* WebView2 startup is asynchronous. */ }
    if (attempt === 99) throw new Error(`No WebView2 debugging endpoint: ${logs}`);
    await delay(250);
  }
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const context = browser.contexts()[0];
  page = context.pages()[0] ?? await context.waitForEvent('page', { timeout: 15000 });
  page.on('pageerror', error => errors.push(error.message));
  await expect(page.locator('#native-badge')).toContainText('LOCAL FIRST', { timeout: 20000 });
  await expect(page.locator('#tabs .tab')).toHaveCount(1);
  await expect(page.locator('#status-left')).toContainText('Connected');
  const second = spawn(executable, [], { windowsHide: true, env: { ...process.env, RELAY_CONFIG_DIR: configDirectory }, stdio: 'ignore' });
  try {
    const exit = new Promise((resolve, reject) => { second.once('error', reject); second.once('exit', resolve); });
    let timer;
    try {
      assert.equal(await Promise.race([exit, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Second instance did not exit')), 10000); })]), 0);
    } finally { clearTimeout(timer); }
  } finally { await stopOwnedChild(second); }
  assert(alive(app.pid));
  await expect(page.locator('#tabs .tab')).toHaveCount(1);
  proof.checks.push('Second launch focuses the existing instance without creating another settings writer');
  await command("Write-Output ('RELAY_' + (6*7) + ' café λ 日本語')", 'RELAY_42 café λ 日本語');
  await command("Write-Output ('RELAY_PID_' + $PID + '_END')", /RELAY_PID_\d+_END/);
  const initialPid = Number((await screen()).match(/RELAY_PID_(\d+)_END/)?.[1]);
  assert(Number.isInteger(initialPid) && initialPid > 0, 'Could not read local shell PID');
  proof.checks.push('Native ConPTY command execution and Unicode input/output');

  await page.locator('#add-tab').click();
  await expect(page.locator('#tabs .tab')).toHaveCount(2);
  await command("Write-Output ('SECOND_' + (7*6))", 'SECOND_42');
  await page.locator('#tabs .tab-select').first().click();
  assert((await screen()).includes('RELAY_42 café λ 日本語'));
  assert(!(await screen()).includes('SECOND_42'));
  proof.checks.push('Independent tab buffers and live session switching');

  const originalGrid = (await page.locator('#status-right').innerText()).match(/^\d+ × \d+/)?.[0];
  assert(originalGrid, 'Could not read the original terminal grid');
  await page.setViewportSize({ width: 1080, height: 720 });
  await expect.poll(async () => {
    const grid = (await page.locator('#status-right').innerText()).match(/^\d+ × \d+/)?.[0];
    return Boolean(grid && grid !== originalGrid);
  }, { timeout: 20000 }).toBe(true);
  const cols = Number((await page.locator('#status-right').innerText()).split(' ')[0]);
  let resizeQuery = 0;
  await expect.poll(async () => {
    const marker = `RELAY_COLS_${++resizeQuery}_`;
    await command(`Write-Output ('${marker}' + $Host.UI.RawUI.WindowSize.Width + '_END')`, new RegExp(`${marker}\\d+_END`));
    return Number((await screen()).match(new RegExp(`${marker}(\\d+)_END`))?.[1]);
  }, { timeout: 20000 }).toBe(cols);
  proof.checks.push('Viewport resizing reaches the native PTY');

  await page.locator('.profile-connect').filter({ hasText: fixture.profile.name }).click();
  await expect(page.locator('#tabs .tab')).toHaveCount(3);
  await expect.poll(screen, { timeout: 20000 }).toContain('Relay SSH fixture ready — café λ');
  await page.locator('#files-toggle').click();
  await expect(page.locator('#remote-files')).toContainText('hello café.txt', { timeout: 20000 });
  await expect(page.locator('#remote-files')).toContainText('nested');
  proof.checks.push('Real SSH authentication and native SFTP directory listing');
  await page.screenshot({ path: path.join(outputDirectory, 'relay-native.png') });

  await page.locator('#new-session').click();
  await page.locator('#connection-form [name="name"]').fill('Saved by native smoke');
  await page.locator('#connection-form [name="host"]').fill('example.invalid');
  await page.locator('#connection-form [name="user"]').fill('testuser');
  await page.locator('#connection-form button[value="save"]').click();
  await expect(page.locator('#profiles')).toContainText('Saved by native smoke');
  const saved = JSON.parse(await readFile(path.join(configDirectory, 'settings.json'), 'utf8'));
  assert(saved.profiles.some(profile => profile.name === 'Saved by native smoke' && profile.host === 'example.invalid'));
  proof.checks.push('Connection form persists validated settings through native IPC');

  await page.locator('#tabs .tab-select').first().click();
  await page.locator('#tabs .tab-close').first().click();
  await expect.poll(() => alive(initialPid), { timeout: 10000 }).toBe(false);
  proof.checks.push('Closing a tab terminates its real shell process');

  await page.locator('#tabs .tab-select').first().click();
  await command("Write-Output ('RELOAD_PID_' + $PID + '_END')", /RELOAD_PID_\d+_END/);
  const reloadPid = Number((await screen()).match(/RELOAD_PID_(\d+)_END/)?.[1]);
  assert(reloadPid > 0);
  await page.reload();
  await expect(page.locator('#tabs .tab')).toHaveCount(1, { timeout: 20000 });
  await expect(page.locator('#profiles')).toContainText('Saved by native smoke');
  await expect.poll(() => alive(reloadPid), { timeout: 10000 }).toBe(false);
  await command("Write-Output ('RELOADED_' + (21*2))", 'RELOADED_42');
  proof.checks.push('Reload closes old sessions, restores profiles, and opens a working shell');
  assert.deepEqual(errors, [], 'Unhandled frontend errors');
  proof.checks.push('No unhandled frontend errors observed');
  proof.passed = true;
  console.log(JSON.stringify(proof));
} catch (error) {
  proof.passed = false;
  proof.error = String(error?.stack ?? error);
  proof.frontendErrors = errors;
  if (page) {
    await page.screenshot({ path: path.join(outputDirectory, 'failure.png') }).catch(() => {});
    await writeFile(path.join(outputDirectory, 'failure-dom.txt'), await page.locator('body').innerText().catch(() => 'Unavailable')).catch(error => console.error(error));
  }
  console.error(proof.error);
  process.exitCode = 1;
} finally {
  try {
    await finishSmoke(async () => {
      await writeFile(path.join(outputDirectory, 'proof.json'), JSON.stringify(proof, null, 2));
      await writeFile(path.join(outputDirectory, 'app.log'), logs);
    }, async () => { if (browser) await browser.close(); }, stopApp);
  } finally { clearTimeout(deadline); }
}
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await runNativeSmoke(process.argv.slice(2)).catch(error => { console.error(error); process.exitCode = 1; });
}
