import './tool-env.mjs';
import { spawn, execFile } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export async function stopOwnedChild(child, processGroup = false) {
  if (!child.pid) return;
  const parentExited = child.exitCode !== null || child.signalCode !== null;
  const inheritedPipesOpen = child.stdio.some(stream => stream && !stream.destroyed && !stream.readableEnded && stream.readable);
  if (parentExited && !inheritedPipesOpen) return;
  if (parentExited && process.platform === 'win32') {
    child.unref();
    for (const stream of child.stdio) stream?.destroy?.();
    throw new Error(`Parent ${child.pid} exited while inherited pipes remained open; descendant cleanup could not be verified`);
  }
  if (parentExited && !processGroup) return;
  const closed = new Promise(resolve => child.once('close', resolve));
  if (process.platform === 'win32') {
    const taskkill = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'taskkill.exe');
    await new Promise((resolve, reject) => {
      execFile(taskkill, ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 10000 }, error => {
        if (!error || child.exitCode !== null || child.signalCode !== null) resolve();
        else { child.kill(); reject(error); }
      });
    });
  } else {
    try { process.kill(processGroup ? -child.pid : child.pid, 'SIGKILL'); }
    catch (error) { if (error.code !== 'ESRCH') throw error; }
  }
  let timer;
  try {
    await Promise.race([closed, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Child ${child.pid} did not close after termination`)), 10000);
    })]);
  } finally { clearTimeout(timer); }
}

export async function runFiniteChild(program, args, { timeoutMs = 600000, stdio = 'inherit', env = process.env } = {}) {
  const processGroup = process.platform !== 'win32';
  const child = spawn(program, args, { stdio, env, windowsHide: true, detached: processGroup });
  let timedOut = false;
  let cleanup;
  let timer;
  const finished = new Promise(resolve => {
    child.once('error', error => resolve({ status: null, error }));
    child.once('close', status => resolve({ status }));
  });
  const deadline = new Promise(resolve => {
    timer = setTimeout(() => {
      timedOut = true;
      cleanup = stopOwnedChild(child, processGroup).catch(error => ({ error }));
      cleanup.then(result => resolve({ status: null, ...result }));
    }, timeoutMs);
  });
  const result = await Promise.race([finished, deadline]);
  clearTimeout(timer);
  const cleanupResult = await cleanup;
  if (cleanupResult?.error) {
    child.unref();
    for (const stream of child.stdio) stream?.destroy?.();
  }
  return { ...result, timedOut, ...(cleanupResult?.error ? { error: cleanupResult.error } : {}) };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = await runFiniteChild('cargo', ['test', '--manifest-path', 'src-tauri/Cargo.toml', ...process.argv.slice(2)]);
  if (result.error) console.error(result.error.message);
  if (result.timedOut) console.error(result.error ? 'Rust tests exceeded their ten-minute deadline; cleanup could not be verified.' : 'Rust tests exceeded their ten-minute deadline; their running process tree was terminated.');
  process.exitCode = result.timedOut || result.error ? 1 : result.status ?? 1;
}
