import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const root = process.cwd();
const resourceDir = path.join(root, 'src-tauri', 'resources');
const platform = `${os.platform()}-${os.arch()}`;
const supportedPlatforms = new Set(['win32-x64', 'darwin-arm64']);
if (!supportedPlatforms.has(platform)) {
  throw new Error(`ChatX desktop packaging currently supports win32-x64 and darwin-arm64; current platform is ${platform}.`);
}

const lockPath = platform === 'win32-x64'
  ? path.join(root, 'runtime-lock.json')
  : path.join(root, `runtime-lock.${platform}.json`);
const launcherSource = path.join(root, 'scripts', 'desktop-commander-launcher.mjs');

if (!fs.existsSync(lockPath)) throw new Error(`Runtime lock is missing for ${platform}: ${lockPath}`);
if (!fs.existsSync(launcherSource)) throw new Error('Desktop Commander launcher source is missing.');
const runtimeLock = JSON.parse(fs.readFileSync(lockPath, 'utf8'));
if (runtimeLock.schemaVersion !== 3 || runtimeLock.platform !== platform) {
  throw new Error(`Runtime lock is for schema/platform ${runtimeLock.schemaVersion}/${runtimeLock.platform ?? 'unknown'}, current is 3/${platform}.`);
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function requireLocked(label, actual, expected) {
  if (actual !== expected) throw new Error(`${label} does not match runtime lock. Expected ${expected}, got ${actual}.`);
}

function run(label, command, args, options = {}) {
  const { env: extraEnv, ...spawnOptions } = options;
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
    env: { ...process.env, ...(extraEnv ?? {}) },
    timeout: 180_000,
    ...spawnOptions,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${label} failed: ${result.error?.message ?? result.stderr ?? result.stdout}`);
  }
  return result;
}

async function download(url, target, label) {
  const response = await fetch(url, { redirect: 'follow' });
  if (!response.ok) throw new Error(`${label} download failed: HTTP ${response.status} ${response.statusText}`);
  fs.writeFileSync(target, Buffer.from(await response.arrayBuffer()));
}

function extractZip(label, archive, destination) {
  fs.mkdirSync(destination, { recursive: true });
  if (process.platform === 'win32') {
    run(
      label,
      'powershell.exe',
      ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
        'Expand-Archive -LiteralPath $env:CHATX_ARCHIVE -DestinationPath $env:CHATX_DEST -Force'],
      { env: { CHATX_ARCHIVE: archive, CHATX_DEST: destination } },
    );
  } else {
    run(label, '/usr/bin/unzip', ['-q', '-o', archive, '-d', destination]);
  }
}

function executableName(base) {
  return process.platform === 'win32' ? `${base}.exe` : base;
}

function ensureExecutable(file) {
  if (process.platform !== 'win32') fs.chmodSync(file, 0o755);
}

function executableVersion(file) {
  const result = run(`${path.basename(file)} --version`, file, ['--version'], { timeout: 10_000 });
  return (result.stdout || result.stderr || '').trim();
}

async function resolveRipgrepBinary(dcRoot) {
  const requireFromDc = createRequire(path.join(dcRoot, 'package.json'));
  const entry = requireFromDc.resolve('@vscode/ripgrep');
  const module = await import(pathToFileURL(entry).href);
  const rgPath = module.rgPath ?? module.default?.rgPath;
  if (typeof rgPath !== 'string' || !rgPath.trim()) {
    throw new Error(`@vscode/ripgrep did not expose rgPath from ${entry}`);
  }
  const resolved = path.resolve(rgPath);
  if (!fs.existsSync(resolved)) throw new Error(`@vscode/ripgrep resolved rgPath but the binary is missing: ${resolved}`);
  return resolved;
}

function validateDesktopCommanderLock() {
  const dc = runtimeLock.desktopCommander ?? {};
  const expectedTag = `v${dc.version}`;
  const expectedAsset = `desktop-commander-${dc.version}.mcpb`;
  const expectedUrl = `https://github.com/wonderwhy-er/DesktopCommanderMCP/releases/download/${expectedTag}/${expectedAsset}`;
  requireLocked('Desktop Commander release tag', dc.releaseTag, expectedTag);
  requireLocked('Desktop Commander release asset', dc.releaseAsset, expectedAsset);
  requireLocked('Desktop Commander release URL', dc.releaseUrl, expectedUrl);
  if (!/^[0-9a-f]{40}$/i.test(dc.sourceCommit ?? '')) throw new Error('Desktop Commander sourceCommit must be a full Git commit SHA.');
  if (!Number.isInteger(dc.size) || dc.size <= 0) throw new Error('Desktop Commander release asset size must be a positive integer.');
  if (!/^[0-9a-f]{64}$/i.test(dc.sha256 ?? '')) throw new Error('Desktop Commander release asset SHA-256 must be pinned.');
}

async function acquireDesktopCommanderArchive() {
  const dc = runtimeLock.desktopCommander;
  const target = path.join(resourceDir, 'desktop-commander-release.zip');
  const configured = process.env.DESKTOP_COMMANDER_MCPB_PATH?.trim();
  if (configured) {
    const source = path.resolve(configured);
    if (!fs.existsSync(source)) throw new Error(`DESKTOP_COMMANDER_MCPB_PATH does not exist: ${source}`);
    fs.copyFileSync(source, target);
  } else {
    await download(dc.releaseUrl, target, 'Desktop Commander release');
  }
  requireLocked('Desktop Commander MCPB size', fs.statSync(target).size, dc.size);
  requireLocked('Desktop Commander MCPB SHA-256', sha256(target), dc.sha256);
  return target;
}

async function prepareNodeRuntime() {
  const target = path.join(resourceDir, executableName('node'));
  if (process.platform === 'win32') {
    requireLocked('Node version', process.version, runtimeLock.node.version);
    requireLocked('Node SHA-256', sha256(process.execPath), runtimeLock.node.sha256);
    fs.copyFileSync(process.execPath, target);
    return { version: process.version, sha256: sha256(target), source: 'locked-local-executable' };
  }

  const node = runtimeLock.node ?? {};
  if (!/^node-v\d+\.\d+\.\d+-darwin-arm64\.tar\.gz$/.test(node.archive ?? '')) throw new Error('darwin-arm64 Node archive name is invalid.');
  if (!/^[0-9a-f]{64}$/i.test(node.archiveSha256 ?? '')) throw new Error('darwin-arm64 Node archive SHA-256 must be pinned.');

  const archive = path.join(resourceDir, node.archive);
  const extractDir = path.join(resourceDir, '.node-extract');
  await download(node.archiveUrl, archive, 'Node.js runtime');
  requireLocked('Node archive SHA-256', sha256(archive), node.archiveSha256);
  fs.mkdirSync(extractDir, { recursive: true });
  try {
    run('Node archive extract', '/usr/bin/tar', ['-xzf', archive, '-C', extractDir]);
    const source = path.join(extractDir, node.archiveRoot, 'bin', 'node');
    if (!fs.existsSync(source)) throw new Error(`Node executable is missing from locked archive: ${source}`);
    fs.copyFileSync(source, target);
    ensureExecutable(target);
  } finally {
    fs.rmSync(extractDir, { recursive: true, force: true });
    fs.rmSync(archive, { force: true });
  }
  requireLocked('Node version', executableVersion(target), node.version);
  return {
    version: node.version,
    sha256: sha256(target),
    source: 'nodejs-release-archive',
    archive: node.archive,
    archiveSha256: node.archiveSha256,
  };
}

function findTunnelClientOnWindows() {
  const configured = process.env.TUNNEL_CLIENT_PATH?.trim();
  if (configured && fs.existsSync(configured)) return configured;
  const localAppData = process.env.LOCALAPPDATA;
  if (localAppData) {
    const installed = path.join(localAppData, 'Programs', 'OpenAI', 'tunnel-client', 'tunnel-client.exe');
    if (fs.existsSync(installed)) return installed;
  }
  const result = spawnSync('where.exe', ['tunnel-client.exe'], { encoding: 'utf8', windowsHide: true });
  if (result.status === 0) {
    const first = result.stdout.split(/\r?\n/).map((value) => value.trim()).find(Boolean);
    if (first && fs.existsSync(first)) return first;
  }
  return null;
}

async function prepareTunnelClientRuntime() {
  const target = path.join(resourceDir, executableName('tunnel-client'));
  if (process.platform === 'win32') {
    const source = findTunnelClientOnWindows();
    if (!source) throw new Error('tunnel-client.exe was not found. Install the locked OpenAI tunnel-client or set TUNNEL_CLIENT_PATH.');
    const version = executableVersion(source);
    requireLocked('tunnel-client version', version, runtimeLock.tunnelClient.version);
    requireLocked('tunnel-client SHA-256', sha256(source), runtimeLock.tunnelClient.sha256);
    fs.copyFileSync(source, target);

    const legalHashes = {};
    for (const name of ['LICENSE', 'NOTICE']) {
      const candidates = [path.join(path.dirname(source), name), path.join(path.dirname(source), `tunnel-client-${name}.txt`)];
      const localAppData = process.env.LOCALAPPDATA;
      if (localAppData) candidates.push(path.join(localAppData, 'Programs', 'OpenAI', 'tunnel-client', name));
      const legalSource = candidates.find((candidate) => fs.existsSync(candidate));
      if (!legalSource) throw new Error(`tunnel-client ${name} file was not found next to the selected binary.`);
      const digest = sha256(legalSource);
      legalHashes[name.toLowerCase()] = digest;
      const lockKey = name === 'LICENSE' ? 'licenseSha256' : 'noticeSha256';
      requireLocked(`tunnel-client ${name} SHA-256`, digest, runtimeLock.tunnelClient[lockKey]);
      fs.copyFileSync(legalSource, path.join(resourceDir, `tunnel-client-${name}.txt`));
    }
    return {
      version,
      sha256: sha256(target),
      source: 'locked-local-executable',
      licenseSha256: legalHashes.license,
      noticeSha256: legalHashes.notice,
    };
  }

  const tunnel = runtimeLock.tunnelClient ?? {};
  if (!/^tunnel-client-v\d+\.\d+\.\d+-darwin-arm64\.zip$/.test(tunnel.releaseAsset ?? '')) throw new Error('darwin-arm64 tunnel-client release asset name is invalid.');
  if (!/^[0-9a-f]{64}$/i.test(tunnel.sha256 ?? '')) throw new Error('darwin-arm64 tunnel-client archive SHA-256 must be pinned.');

  const archive = path.join(resourceDir, tunnel.releaseAsset);
  const extractDir = path.join(resourceDir, '.tunnel-client-extract');
  await download(tunnel.releaseUrl, archive, 'OpenAI tunnel-client');
  requireLocked('tunnel-client release size', fs.statSync(archive).size, tunnel.size);
  requireLocked('tunnel-client release SHA-256', sha256(archive), tunnel.sha256);
  try {
    extractZip('tunnel-client release extract', archive, extractDir);
    for (const name of ['tunnel-client', 'cloudflared', 'cloudflared-manifest.json', 'LICENSE', 'NOTICE']) {
      if (!fs.existsSync(path.join(extractDir, name))) throw new Error(`tunnel-client release archive is missing ${name}`);
    }
    fs.copyFileSync(path.join(extractDir, 'tunnel-client'), target);
    fs.copyFileSync(path.join(extractDir, 'cloudflared'), path.join(resourceDir, 'cloudflared'));
    fs.copyFileSync(path.join(extractDir, 'cloudflared-manifest.json'), path.join(resourceDir, 'cloudflared-manifest.json'));
    fs.copyFileSync(path.join(extractDir, 'LICENSE'), path.join(resourceDir, 'tunnel-client-LICENSE.txt'));
    fs.copyFileSync(path.join(extractDir, 'NOTICE'), path.join(resourceDir, 'tunnel-client-NOTICE.txt'));
    ensureExecutable(target);
    ensureExecutable(path.join(resourceDir, 'cloudflared'));
  } finally {
    fs.rmSync(extractDir, { recursive: true, force: true });
    fs.rmSync(archive, { force: true });
  }
  const version = executableVersion(target);
  requireLocked('tunnel-client version', version, tunnel.version);
  return {
    version,
    sha256: sha256(target),
    source: 'github-release-archive',
    releaseTag: tunnel.releaseTag,
    releaseAsset: tunnel.releaseAsset,
    releaseSha256: tunnel.sha256,
    cloudflaredSha256: sha256(path.join(resourceDir, 'cloudflared')),
    cloudflaredManifestSha256: sha256(path.join(resourceDir, 'cloudflared-manifest.json')),
    licenseSha256: sha256(path.join(resourceDir, 'tunnel-client-LICENSE.txt')),
    noticeSha256: sha256(path.join(resourceDir, 'tunnel-client-NOTICE.txt')),
  };
}

validateDesktopCommanderLock();
fs.rmSync(resourceDir, { recursive: true, force: true });
fs.mkdirSync(resourceDir, { recursive: true });
fs.copyFileSync(launcherSource, path.join(resourceDir, 'desktop-commander-launcher.mjs'));

const nodeMetadata = await prepareNodeRuntime();
const tunnelMetadata = await prepareTunnelClientRuntime();

const dcRoot = path.join(resourceDir, 'desktop-commander');
fs.mkdirSync(dcRoot, { recursive: true });
const dcArchive = await acquireDesktopCommanderArchive();
try {
  extractZip('Desktop Commander MCPB extract', dcArchive, dcRoot);
} finally {
  fs.rmSync(dcArchive, { force: true });
}

const dcManifestPath = path.join(dcRoot, 'manifest.json');
const dcPackagePath = path.join(dcRoot, 'package.json');
const dcEntry = path.join(dcRoot, 'dist', 'index.js');
for (const file of [dcManifestPath, dcPackagePath, dcEntry]) {
  if (!fs.existsSync(file)) throw new Error(`Desktop Commander release bundle is incomplete: ${file}`);
}

const dcManifest = JSON.parse(fs.readFileSync(dcManifestPath, 'utf8'));
const dcPackage = JSON.parse(fs.readFileSync(dcPackagePath, 'utf8'));
requireLocked('Desktop Commander MCPB manifest version', dcManifest.version, runtimeLock.desktopCommander.version);
requireLocked('Desktop Commander package version', dcPackage.version, runtimeLock.desktopCommander.version);

const ripgrep = await resolveRipgrepBinary(dcRoot);
const ripgrepRelative = path.relative(resourceDir, ripgrep).split(path.sep).join('/');
if (!ripgrepRelative || ripgrepRelative === '..' || ripgrepRelative.startsWith('../')) {
  throw new Error(`Resolved ripgrep binary is outside the bundled resource directory: ${ripgrep}`);
}
ensureExecutable(ripgrep);

const dcLicense = path.join(dcRoot, 'LICENSE');
if (!fs.existsSync(dcLicense)) throw new Error('Desktop Commander LICENSE was not included in the locked MCPB release asset.');
fs.copyFileSync(dcLicense, path.join(resourceDir, 'DesktopCommander-LICENSE.txt'));

const manifest = {
  schemaVersion: 3,
  platform,
  runtimePolicy: 'locked-github-release',
  node: nodeMetadata,
  tunnelClient: tunnelMetadata,
  desktopCommander: {
    version: dcPackage.version,
    source: 'github-release-mcpb',
    sourceCommit: runtimeLock.desktopCommander.sourceCommit,
    releaseTag: runtimeLock.desktopCommander.releaseTag,
    releaseAsset: runtimeLock.desktopCommander.releaseAsset,
    releaseSize: runtimeLock.desktopCommander.size,
    releaseSha256: runtimeLock.desktopCommander.sha256,
    bundleManifestSha256: sha256(dcManifestPath),
    entry: 'desktop-commander/dist/index.js',
    launcher: 'desktop-commander-launcher.mjs',
    telemetryDisabledByEnv: true,
    runtimeKeyStrippedByLauncher: true,
    ripgrep: ripgrepRelative,
    ripgrepSha256: sha256(ripgrep),
    license: 'DesktopCommander-LICENSE.txt',
  },
};
fs.writeFileSync(path.join(resourceDir, 'runtime-manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');

console.log(`desktop resources prepared: ${resourceDir}`);
console.log(`  platform: ${platform}`);
console.log('  architecture: tunnel-client -> stdio -> Desktop Commander');
console.log(`  node: ${nodeMetadata.version}`);
console.log(`  tunnel-client: ${tunnelMetadata.version}`);
console.log(`  desktop-commander: ${dcPackage.version} (${runtimeLock.desktopCommander.releaseAsset})`);
console.log(`  desktop-commander release SHA-256: ${runtimeLock.desktopCommander.sha256}`);
console.log(`  ripgrep: ${ripgrep}`);
