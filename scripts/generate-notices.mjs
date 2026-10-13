import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const platform = 'x86_64-pc-windows-msvc';
const output = path.join(root, 'THIRD_PARTY_NOTICES.txt');
const checksumOutput = path.join(root, 'THIRD_PARTY_NOTICES.sha256');
const licenseName = /^(licen[sc]es?|copying|copyright|notices?)([._-]|$)/i;
const markers = {
  MIT: /permission is hereby granted, free of charge/, 'MIT-0': /permission is hereby granted, free of charge/,
  'Apache-2.0': /apache license.*version 2\.0.*terms and conditions/,
  'BSD-2-Clause': /redistribution and use in source and binary forms.*redistributions of source code.*redistributions in binary form/,
  'BSD-3-Clause': /redistribution and use in source and binary forms.*redistributions of source code.*redistributions in binary form.*neither/,
  'MPL-2.0': /mozilla public license.*version 2\.0.*definitions.*license grants/,
  'Unicode-3.0': /unicode.*(?:license v3|permission notice).*permission is hereby granted/,
  Zlib: /this software is provided.*as.is.*altered source versions/,
  Unlicense: /free and unencumbered software released into the public domain/,
  '0BSD': /permission to use, copy, modify, and.or distribute this software for any purpose with or without fee/,
  'CC0-1.0': /creative commons.*cc0.*copyright and related rights/,
};
const remoteTrees = new Map();
const remoteText = new Map();
const sha256 = text => createHash('sha256').update(text).digest('hex');
const normalized = text => text.replace(/\r\n?/g, '\n').trimEnd();
const lf = text => text.replace(/\r\n?/g, '\n');

export async function checkNotices(directory = root) {
  const notices = lf(await readFile(path.join(directory, 'THIRD_PARTY_NOTICES.txt'), 'utf8'));
  const checksum = (await readFile(path.join(directory, 'THIRD_PARTY_NOTICES.sha256'), 'utf8')).trim();
  if (checksum !== `${sha256(notices)}  THIRD_PARTY_NOTICES.txt`) throw new Error('Third-party notice integrity check failed. Regenerate and review the notices before packaging.');
  const cargoLock = lf(await readFile(path.join(directory, 'src-tauri/Cargo.lock'), 'utf8'));
  const npmLock = lf(await readFile(path.join(directory, 'package-lock.json'), 'utf8'));
  for (const [name, contents] of [['Cargo.lock', cargoLock], ['package-lock.json', npmLock]]) {
    if (!notices.includes(`${name} SHA-256 (LF line endings): ${sha256(contents)}\n`)) throw new Error(`${name} changed. Regenerate and review third-party notices before packaging.`);
  }
  if (!notices.startsWith(`Relay third-party notices\nTarget: ${platform}\n`)) throw new Error('Unexpected notices target or format.');
  const cargoPackages = new Set(cargoLock.split('[[package]]').slice(1).map(block => {
    const name = block.match(/^name = "([^"]+)"$/m)?.[1];
    const version = block.match(/^version = "([^"]+)"$/m)?.[1];
    if (!name || !version) throw new Error('Cannot read a Cargo.lock package.');
    return `${name}@${version}`;
  }));
  const npm = JSON.parse(npmLock);
  const index = new Map();
  const counts = { Cargo: 0, npm: 0 };
  for (const section of notices.split(`\n${'='.repeat(78)}\n`).slice(1)) {
    const header = section.match(/^Package: (\S+) (\S+)\nEcosystem: (Cargo|npm)\nSource: (\S+)\nDeclared license: ([^\n]+)\n/);
    if (!header) throw new Error('Malformed third-party package index.');
    const [, name, version, ecosystem, source, license] = header;
    const key = `${ecosystem}:${name}@${version}`;
    if (index.has(key)) throw new Error(`Duplicate third-party package: ${key}`);
    if (!licenseCovered(license, [{ text: section }])) throw new Error(`License text missing for ${key}.`);
    index.set(key, true); counts[ecosystem]++;
    if (ecosystem === 'Cargo') {
      if (!cargoPackages.has(`${name}@${version}`) || source !== `https://crates.io/crates/${name}/${version}`) throw new Error(`Cargo notice does not match the lockfile: ${key}`);
    } else {
      const locked = npm.packages?.[`node_modules/${name}`];
      if (locked?.version !== version || locked.resolved !== source) throw new Error(`Frontend notice does not match the lockfile: ${key}`);
    }
  }
  const pending = Object.keys(npm.packages?.['']?.dependencies || {}); const seen = new Set();
  for (const name of pending) {
    if (seen.has(name)) continue;
    seen.add(name);
    const locked = npm.packages?.[`node_modules/${name}`];
    if (!locked || !index.has(`npm:${name}@${locked.version}`)) throw new Error(`Bundled frontend package has no notice: ${name}`);
    pending.push(...Object.keys(locked.dependencies || {}));
  }
  if (seen.size !== counts.npm || !notices.includes(`Packages: ${counts.Cargo} Cargo; ${counts.npm} npm\n`)) throw new Error('Third-party notice package coverage or counts do not match.');
  return { cargo: counts.Cargo, npm: counts.npm, bytes: Buffer.byteLength(notices) };
}

function licenseCovered(expression, documents) {
  if (typeof expression !== 'string' || !expression.trim()) throw new Error('Missing declared license; review required.');
  const tokens = expression.replaceAll('/', ' OR ').match(/[A-Za-z0-9.+-]+|[()]/g) ?? [];
  for (const token of tokens) if (!['OR', 'AND', '(', ')'].includes(token) && !markers[token]) throw new Error(`Unknown license ${token}; review required.`);
  const content = documents.map(document => document.text).join('\n').replace(/\s+/g, ' ').toLowerCase();
  let position = 0;
  function term() {
    const token = tokens[position++];
    if (token === '(') { const covered = or(); if (tokens[position++] !== ')') throw new Error(`Malformed license expression: ${expression}`); return covered; }
    if (!markers[token]) throw new Error(`Malformed license expression: ${expression}`);
    return markers[token].test(content);
  }
  function and() { let covered = term(); while (tokens[position] === 'AND') { position++; const next = term(); covered = covered && next; } return covered; }
  function or() { let covered = and(); while (tokens[position] === 'OR') { position++; const next = and(); covered = covered || next; } return covered; }
  const covered = or();
  if (position !== tokens.length) throw new Error(`Malformed license expression: ${expression}`);
  return covered;
}

async function readDocuments(directory, declaredFile) {
  const documents = [];
  const visited = new Set();
  async function add(relative) {
    const absolute = path.resolve(directory, relative);
    if (!absolute.startsWith(`${directory}${path.sep}`)) throw new Error('License file escapes its package directory.');
    const label = relative.split(path.sep).join('/');
    if (visited.has(label)) return;
    visited.add(label);
    const text = normalized(await readFile(absolute, 'utf8'));
    if (!text || text.includes('\0') || text.includes('\uFFFD')) throw new Error(`Unusable license document: ${label}`);
    documents.push({ name: label, text });
  }
  async function scan(relative = '', depth = 0) {
    for (const entry of await readdir(path.join(directory, relative), { withFileTypes: true })) {
      if (!relative && !licenseName.test(entry.name)) continue;
      const child = path.join(relative, entry.name);
      if (entry.isFile()) await add(child);
      else if (entry.isDirectory()) {
        if (depth >= 8) throw new Error('License directory nesting exceeds supported limits.');
        await scan(child, depth + 1);
      }
    }
  }
  await scan();
  if (declaredFile) await add(declaredFile);
  return documents;
}

async function fetchText(url) {
  if (!remoteText.has(url)) remoteText.set(url, (async () => {
    const response = await fetch(url, { signal: AbortSignal.timeout(15_000), headers: { 'User-Agent': 'relay-terminal-license-notices' } });
    if (!response.ok) throw new Error(`Cannot retrieve upstream license (${response.status}): ${url}`);
    const text = normalized(await response.text());
    if (!text || text.includes('\0') || text.includes('\uFFFD') || /<!doctype html/i.test(text)) throw new Error(`Unusable upstream license: ${url}`);
    return text;
  })());
  return remoteText.get(url);
}

async function upstreamDocuments(crate) {
  const directory = path.dirname(crate.manifest_path);
  const vcs = JSON.parse(await readFile(path.join(directory, '.cargo_vcs_info.json'), 'utf8'));
  const repository = crate.repository?.match(/^https:\/\/github\.com\/([^/]+\/[^/]+?)(?:\.git)?\/?$/)?.[1];
  const commit = vcs.git?.sha1;
  if (!repository || !/^[a-f0-9]{40}$/.test(commit)) throw new Error(`No immutable upstream license source for ${crate.name}.`);
  const treeUrl = `https://api.github.com/repos/${repository}/git/trees/${commit}?recursive=1`;
  if (!remoteTrees.has(treeUrl)) remoteTrees.set(treeUrl, fetchText(treeUrl).then(JSON.parse));
  const tree = await remoteTrees.get(treeUrl);
  if (tree.truncated || !Array.isArray(tree.tree)) throw new Error(`Incomplete upstream tree for ${crate.name}.`);
  const ancestors = new Set(['']);
  let current = vcs.path_in_vcs || '';
  while (current) { ancestors.add(current); current = path.posix.dirname(current); if (current === '.') current = ''; }
  const files = tree.tree.filter(entry => entry.type === 'blob' && licenseName.test(path.posix.basename(entry.path)) && ancestors.has(path.posix.dirname(entry.path) === '.' ? '' : path.posix.dirname(entry.path)));
  const documents = [];
  for (const file of files.sort((a, b) => a.path.localeCompare(b.path, 'en'))) {
    const url = `https://raw.githubusercontent.com/${repository}/${commit}/${file.path}`;
    documents.push({ name: file.path, source: url, text: await fetchText(url) });
  }
  if (crate.name === 'selectors' && crate.license === 'MPL-2.0' && !documents.length) {
    const source = await readFile(path.join(directory, 'lib.rs'), 'utf8');
    const notice = source.match(/^\/\*[\s\S]*?\*\//)?.[0];
    if (!notice?.includes('https://mozilla.org/MPL/2.0/')) throw new Error('selectors no longer references the expected MPL license. Review required.');
    documents.push({ name: 'lib.rs source license notice', text: notice });
    const url = 'https://www.mozilla.org/media/MPL/2.0/index.txt';
    documents.push({ name: 'Mozilla Public License 2.0', source: url, text: await fetchText(url) });
  }
  return documents;
}

async function main() {
  if (process.argv.slice(2).some(argument => argument !== '--check')) throw new Error('Usage: node scripts/generate-notices.mjs [--check]');
  if (process.argv.includes('--check')) {
    const checked = await checkNotices();
    console.log(`Third-party notices verified offline: ${checked.cargo} Cargo packages, ${checked.npm} npm packages; ${checked.bytes} bytes.`);
    return;
  }
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--format-version', '1', '--filter-platform', platform, '--manifest-path', 'src-tauri/Cargo.toml'], { cwd: root, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, timeout: 60_000 }));
  const resolved = new Set(metadata.resolve.nodes.map(node => node.id));
  const crates = metadata.packages.filter(crate => resolved.has(crate.id) && crate.source).sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`, 'en'));
  const lock = JSON.parse(await readFile(path.join(root, 'package-lock.json'), 'utf8'));
  const packages = [];
  for (const crate of crates) {
    let documents = await readDocuments(path.dirname(crate.manifest_path), crate.license_file);
    if (!licenseCovered(crate.license, documents)) documents = [...documents, ...await upstreamDocuments(crate)];
    if (!licenseCovered(crate.license, documents)) throw new Error(`Full license text missing for Cargo ${crate.name} ${crate.version}: ${crate.license}`);
    packages.push({ name: crate.name, version: crate.version, ecosystem: 'Cargo', source: `https://crates.io/crates/${crate.name}/${crate.version}`, license: crate.license, documents });
  }
  const pending = Object.keys(lock.packages?.['']?.dependencies || {}); const seen = new Set();
  for (const name of pending) {
    if (seen.has(name)) continue;
    seen.add(name);
    const directory = path.join(root, 'node_modules', name);
    const pkg = JSON.parse(await readFile(path.join(directory, 'package.json'), 'utf8'));
    const locked = lock.packages[`node_modules/${name}`];
    if (pkg.name !== name || pkg.version !== locked?.version || !locked.resolved?.startsWith('https://registry.npmjs.org/')) throw new Error(`Installed frontend package does not match its registry lock: ${name}`);
    let documents = await readDocuments(directory);
    if (!licenseCovered(pkg.license, documents) && name === '@tauri-apps/plugin-dialog') {
      const companion = packages.find(pkg => pkg.ecosystem === 'Cargo' && pkg.name === 'tauri-plugin-dialog' && pkg.version === locked.version);
      if (!companion) throw new Error('Matching native dialog package license unavailable.');
      documents = [...documents, ...companion.documents.filter(document => !document.name.endsWith('.spdx')).map(document => ({ ...document, name: `tauri-plugin-dialog ${locked.version}/${document.name}`, source: `${companion.source} (package license ${document.name})` }))];
    }
    if (!licenseCovered(pkg.license, documents)) throw new Error(`Full license text missing for npm ${name} ${pkg.version}: ${pkg.license}`);
    packages.push({ name, version: pkg.version, ecosystem: 'npm', source: locked.resolved, license: pkg.license, documents });
    pending.push(...Object.keys(pkg.dependencies || {}));
  }
  const cargoLock = (await readFile(path.join(root, 'src-tauri/Cargo.lock'), 'utf8')).replace(/\r\n?/g, '\n');
  const npmLock = (await readFile(path.join(root, 'package-lock.json'), 'utf8')).replace(/\r\n?/g, '\n');
  const sections = [
    'Relay third-party notices',
    `Target: ${platform}`,
    'Scope: the locked Windows Cargo dependency graph, including build dependencies, plus bundled frontend runtime packages. Build-only dependencies are included conservatively.',
    'Third-party code retains the licenses and copyright notices below. Package source code is available at each Source URL. Relay does not modify these third-party package sources.',
    `Cargo.lock SHA-256 (LF line endings): ${sha256(cargoLock)}`,
    `package-lock.json SHA-256 (LF line endings): ${sha256(npmLock)}`,
    `Packages: ${crates.length} Cargo; ${seen.size} npm`,
    'Regenerate: node scripts/generate-notices.mjs',
  ];
  for (const pkg of packages) {
    sections.push(`\n${'='.repeat(78)}\nPackage: ${pkg.name} ${pkg.version}\nEcosystem: ${pkg.ecosystem}\nSource: ${pkg.source}\nDeclared license: ${pkg.license}`);
    for (const document of pkg.documents.sort((a, b) => a.name.localeCompare(b.name, 'en'))) sections.push(`\n--- ${document.name} ---${document.source ? `\nRetrieved from: ${document.source}` : ''}\nSHA-256: ${sha256(document.text)}\n\n${document.text}`);
  }
  const notices = `${sections.join('\n')}\n`;
  await writeFile(output, notices, 'utf8');
  await writeFile(checksumOutput, `${sha256(notices)}  THIRD_PARTY_NOTICES.txt\n`, 'utf8');
  console.log(`Third-party notices generated: ${crates.length} Cargo packages, ${seen.size} npm packages; ${Buffer.byteLength(notices)} bytes.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
