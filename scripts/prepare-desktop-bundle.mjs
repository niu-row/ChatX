import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const root = process.cwd();
const resourceDir = path.join(root, 'src-tauri', 'resources');
const lockPath = path.join(root, 'runtime-lock.json');
const launcherSource = path.join(root, 'scripts', 'desktop-commander-launcher.mjs');

if (!fs.existsSync(lockPath)) throw new Error('runtime-lock.json is required for desktop packaging.');
if (!fs.existsSync(launcherSource)) throw new Error('Desktop Commander launcher source is missing.');
const runtimeLock = JSON.parse(fs.readFileSync(lockPath, 'utf8'));
const platform = `${os.platform()}-${os.arch()}`;
if (runtimeLock.schemaVersion !== 3 || runtimeLock.platform !== platform) {
  throw new Error(`Runtime lock is for schema/platform ${runtimeLock.schemaVersion}/${runtimeLock.platform ?? 'unknown'}, current is 3/${platform}.`);
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function requireLocked(label, actual, expected) {
  if (actual !== expected) throw new Error(`${label} does not match runtime-lock.json. Expected ${expected}, got ${actual}.`);
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

async function resolveRipgrepBinary(dcPackageRoot) {
  const requireFromDc = createRequire(path.join(dcPackageRoot, 'package.json'));
  const entry = requireFromDc.resolve('@vscode/ripgrep');
  const module = await import(pathToFileURL(entry).href);
  const rgPath = module.rgPath ?? module.default?.rgPath;
  if (typeof rgPath !== 'string' || !rgPath.trim()) {
    throw new Error(`@vscode/ripgrep did not expose rgPath from ${entry}`);
  }
  const resolved = path.resolve(rgPath);
  if (!fs.existsSync(resolved)) {
    throw new Error(`@vscode/ripgrep resolved rgPath but the binary is missing: ${resolved}`);
  }
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
  if (!/^[0-9a-f]{40}$/i.test(dc.sourceCommit ?? '')) {
    throw new Error('Desktop Commander sourceCommit must be a full Git commit SHA.');
  }
  if (!Number.isInteger(dc.size) || dc.size <= 0) {
    throw new Error('Desktop Commander release asset size must be a positive integer.');
  }
  if (!/^[0-9a-f]{64}$/i.test(dc.sha256 ?? '')) {
    throw new Error('Desktop Commander release asset SHA-256 must be pinned.');
  }
}

async function acquireDesktopCommanderArchive() {
  const dc = runtimeLock.desktopCommander;
  const target = path.join(resourceDir, 'desktop-commander-release.zip');
  const configured = process.env.DESKTOP_COMMANDER_MCPB_PATH?.trim();

  if (configured) {
    const source = path.resolve(configured);
    if (!fs.existsSync(source)) {
      throw new Error(`DESKTOP_COMMANDER_MCPB_PATH does not exist: ${source}`);
    }
    fs.copyFileSync(source, target);
  } else {
    const response = await fetch(dc.releaseUrl, { redirect: 'follow' });
    if (!response.ok) {
      throw new Error(`Desktop Commander release download failed: HTTP ${response.status} ${response.statusText}`);
    }
    const bytes = Buffer.from(await response.arrayBuffer());
    fs.writeFileSync(target, bytes);
  }

  requireLocked('Desktop Commander MCPB size', fs.statSync(target).size, dc.size);
  requireLocked('Desktop Commander MCPB SHA-256', sha256(target), dc.sha256);
  return target;
}

requireLocked('Node version', process.version, runtimeLock.node.version);
requireLocked('Node SHA-256', sha256(process.execPath), runtimeLock.node.sha256);
validateDesktopCommanderLock();

if (process.platform !== 'win32') {
  throw new Error('The current desktop packaging workflow is configured for Windows.');
}

fs.rmSync(resourceDir, { recursive: true, force: true });
fs.mkdirSync(resourceDir, { recursive: true });

const nodeTarget = path.join(resourceDir, 'node.exe');
fs.copyFileSync(process.execPath, nodeTarget);
fs.copyFileSync(launcherSource, path.join(resourceDir, 'desktop-commander-launcher.mjs'));

function findTunnelClient() {
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

const tunnelSource = findTunnelClient();
if (!tunnelSource) throw new Error('tunnel-client.exe was not found. Install the locked OpenAI tunnel-client or set TUNNEL_CLIENT_PATH.');
const tunnelVersionResult = spawnSync(tunnelSource, ['--version'], { encoding: 'utf8', windowsHide: true, timeout: 5000 });
if (tunnelVersionResult.error || tunnelVersionResult.status !== 0) {
  throw new Error(`Unable to read tunnel-client version: ${tunnelVersionResult.error?.message ?? tunnelVersionResult.stderr}`);
}
const tunnelVersion = (tunnelVersionResult.stdout || tunnelVersionResult.stderr || '').trim();
requireLocked('tunnel-client version', tunnelVersion, runtimeLock.tunnelClient.version);
requireLocked('tunnel-client SHA-256', sha256(tunnelSource), runtimeLock.tunnelClient.sha256);
const tunnelTarget = path.join(resourceDir, 'tunnel-client.exe');
fs.copyFileSync(tunnelSource, tunnelTarget);

const legalHashes = {};
for (const name of ['LICENSE', 'NOTICE']) {
  const candidates = [
    path.join(path.dirname(tunnelSource), name),
    path.join(path.dirname(tunnelSource), `tunnel-client-${name}.txt`),
  ];
  const localAppData = process.env.LOCALAPPDATA;
  if (localAppData) candidates.push(path.join(localAppData, 'Programs', 'OpenAI', 'tunnel-client', name));
  const source = candidates.find((candidate) => fs.existsSync(candidate));
  if (!source) throw new Error(`tunnel-client ${name} file was not found next to the selected binary.`);
  const digest = sha256(source);
  legalHashes[name.toLowerCase()] = digest;
  const lockKey = name === 'LICENSE' ? 'licenseSha256' : 'noticeSha256';
  requireLocked(`tunnel-client ${name} SHA-256`, digest, runtimeLock.tunnelClient[lockKey]);
  fs.copyFileSync(source, path.join(resourceDir, `tunnel-client-${name}.txt`));
}

const dcRoot = path.join(resourceDir, 'desktop-commander');
const dcPackageRoot = path.join(dcRoot, 'node_modules', '@wonderwhy-er', 'desktop-commander');
fs.mkdirSync(dcPackageRoot, { recursive: true });
const dcArchive = await acquireDesktopCommanderArchive();
try {
  run(
    'Desktop Commander MCPB extract',
    'powershell.exe',
    ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', 'Expand-Archive -LiteralPath $env:CHATX_MCPB_ARCHIVE -DestinationPath $env:CHATX_MCPB_DEST -Force'],
    { env: { CHATX_MCPB_ARCHIVE: dcArchive, CHATX_MCPB_DEST: dcPackageRoot } },
  );
} finally {
  fs.rmSync(dcArchive, { force: true });
}

const dcManifestPath = path.join(dcPackageRoot, 'manifest.json');
const dcPackagePath = path.join(dcPackageRoot, 'package.json');
const dcEntry = path.join(dcPackageRoot, 'dist', 'index.js');
for (const file of [dcManifestPath, dcPackagePath, dcEntry]) {
  if (!fs.existsSync(file)) throw new Error(`Desktop Commander release bundle is incomplete: ${file}`);
}

const dcManifest = JSON.parse(fs.readFileSync(dcManifestPath, 'utf8'));
const dcPackage = JSON.parse(fs.readFileSync(dcPackagePath, 'utf8'));
requireLocked('Desktop Commander MCPB manifest version', dcManifest.version, runtimeLock.desktopCommander.version);
requireLocked('Desktop Commander package version', dcPackage.version, runtimeLock.desktopCommander.version);

const ripgrep = await resolveRipgrepBinary(dcPackageRoot);
const ripgrepRelative = path.relative(resourceDir, ripgrep).split(path.sep).join('/');
if (!ripgrepRelative || ripgrepRelative === '..' || ripgrepRelative.startsWith('../')) {
  throw new Error(`Resolved ripgrep binary is outside the bundled resource directory: ${ripgrep}`);
}

const dcLicense = path.join(dcPackageRoot, 'LICENSE');
if (!fs.existsSync(dcLicense)) throw new Error('Desktop Commander LICENSE was not included in the locked MCPB release asset.');
fs.copyFileSync(dcLicense, path.join(resourceDir, 'DesktopCommander-LICENSE.txt'));

const manifest = {
  schemaVersion: 3,
  platform,
  runtimePolicy: 'locked-github-release',
  node: { version: process.version, sha256: sha256(nodeTarget) },
  tunnelClient: {
    version: tunnelVersion,
    sha256: sha256(tunnelTarget),
    licenseSha256: legalHashes.license,
    noticeSha256: legalHashes.notice,
  },
  desktopCommander: {
    version: dcPackage.version,
    source: 'github-release-mcpb',
    sourceCommit: runtimeLock.desktopCommander.sourceCommit,
    releaseTag: runtimeLock.desktopCommander.releaseTag,
    releaseAsset: runtimeLock.desktopCommander.releaseAsset,
    releaseSize: runtimeLock.desktopCommander.size,
    releaseSha256: runtimeLock.desktopCommander.sha256,
    bundleManifestSha256: sha256(dcManifestPath),
    entry: 'desktop-commander/node_modules/@wonderwhy-er/desktop-commander/dist/index.js',
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
console.log('  architecture: tunnel-client -> stdio -> Desktop Commander');
console.log(`  node: ${process.version}`);
console.log(`  tunnel-client: ${tunnelVersion}`);
console.log(`  desktop-commander: ${dcPackage.version} (${runtimeLock.desktopCommander.releaseAsset})`);
console.log(`  desktop-commander release SHA-256: ${runtimeLock.desktopCommander.sha256}`);
console.log(`  ripgrep: ${ripgrep}`);
