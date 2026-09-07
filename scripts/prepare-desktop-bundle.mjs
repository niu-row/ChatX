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
if (runtimeLock.schemaVersion !== 2 || runtimeLock.platform !== platform) {
  throw new Error(`Runtime lock is for schema/platform ${runtimeLock.schemaVersion}/${runtimeLock.platform ?? 'unknown'}, current is 2/${platform}.`);
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function requireLocked(label, actual, expected) {
  if (actual !== expected) throw new Error(`${label} does not match runtime-lock.json. Expected ${expected}, got ${actual}.`);
}

function run(label, command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
    env: { ...process.env, npm_config_update_notifier: 'false' },
    timeout: 180_000,
    ...options,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${label} failed: ${result.error?.message ?? result.stderr ?? result.stdout}`);
  }
  return result;
}

function resolveNpmCli() {
  const candidates = [
    process.env.npm_execpath?.trim(),
    path.join(path.dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js'),
  ].filter(Boolean);

  for (const candidate of candidates) {
    if (candidate && fs.existsSync(candidate)) return path.resolve(candidate);
  }

  throw new Error(
    'npm CLI entry was not found. Run this script through npm (npm run desktop:prepare) or use a Node installation that includes npm.',
  );
}

const npmCli = resolveNpmCli();
function runNpm(label, args, options = {}) {
  // Do not spawn npm.cmd directly. Node 24 on Windows can return EINVAL for
  // direct .cmd execution via spawnSync. Running npm-cli.js with the current
  // node executable also avoids cmd.exe quoting issues for paths with spaces.
  return run(label, process.execPath, [npmCli, ...args], options);
}

async function resolveRipgrepBinary(dcRoot) {
  // @vscode/ripgrep changed packaging in 1.18: older releases downloaded to
  // @vscode/ripgrep/bin/rg.exe, while newer releases expose a binary from a
  // platform-specific optional package. Use the package's public rgPath export
  // instead of assuming either internal layout.
  const requireFromDc = createRequire(path.join(dcRoot, 'package.json'));
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

requireLocked('Node version', process.version, runtimeLock.node.version);
requireLocked('Node SHA-256', sha256(process.execPath), runtimeLock.node.sha256);

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
fs.mkdirSync(dcRoot, { recursive: true });
const packageSpec = `@wonderwhy-er/desktop-commander@${runtimeLock.desktopCommander.version}`;
runNpm('Desktop Commander install', [
  'install',
  '--prefix', dcRoot,
  '--omit=dev',
  '--no-audit',
  '--no-fund',
  '--ignore-scripts',
  '--save-exact',
  packageSpec,
]);

// Desktop Commander 0.2.48 allows @vscode/ripgrep ^1.15.9. Rebuild supports
// the legacy package that downloads rg in postinstall; newer 1.18+ packages
// have no postinstall and resolve rg from a platform-specific optional package.
runNpm('Desktop Commander ripgrep rebuild', [
  'rebuild',
  '--prefix', dcRoot,
  '--no-audit',
  '--no-fund',
  '@vscode/ripgrep',
]);

const dcPackageRoot = path.join(dcRoot, 'node_modules', '@wonderwhy-er', 'desktop-commander');
const dcPackagePath = path.join(dcPackageRoot, 'package.json');
const dcEntry = path.join(dcPackageRoot, 'dist', 'index.js');
if (!fs.existsSync(dcPackagePath) || !fs.existsSync(dcEntry)) {
  throw new Error('Desktop Commander package is incomplete after npm install.');
}
const ripgrep = await resolveRipgrepBinary(dcRoot);
const ripgrepRelative = path.relative(resourceDir, ripgrep).split(path.sep).join('/');
if (!ripgrepRelative || ripgrepRelative === '..' || ripgrepRelative.startsWith('../')) {
  throw new Error(`Resolved ripgrep binary is outside the bundled resource directory: ${ripgrep}`);
}

const dcPackage = JSON.parse(fs.readFileSync(dcPackagePath, 'utf8'));
requireLocked('Desktop Commander version', dcPackage.version, runtimeLock.desktopCommander.version);
const dcLicense = path.join(dcPackageRoot, 'LICENSE');
if (!fs.existsSync(dcLicense)) throw new Error('Desktop Commander LICENSE was not included by npm.');
fs.copyFileSync(dcLicense, path.join(resourceDir, 'DesktopCommander-LICENSE.txt'));

const dcInstallLock = path.join(dcRoot, 'package-lock.json');
const manifest = {
  schemaVersion: 2,
  platform,
  runtimePolicy: 'locked-top-level',
  node: { version: process.version, sha256: sha256(nodeTarget) },
  tunnelClient: {
    version: tunnelVersion,
    sha256: sha256(tunnelTarget),
    licenseSha256: legalHashes.license,
    noticeSha256: legalHashes.notice,
  },
  desktopCommander: {
    version: dcPackage.version,
    entry: 'desktop-commander/node_modules/@wonderwhy-er/desktop-commander/dist/index.js',
    launcher: 'desktop-commander-launcher.mjs',
    telemetryDisabledByEnv: true,
    ripgrep: ripgrepRelative,
    ripgrepSha256: sha256(ripgrep),
    installLockSha256: fs.existsSync(dcInstallLock) ? sha256(dcInstallLock) : null,
    license: 'DesktopCommander-LICENSE.txt',
  },
};
fs.writeFileSync(path.join(resourceDir, 'runtime-manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');

console.log(`desktop resources prepared: ${resourceDir}`);
console.log('  architecture: tunnel-client -> stdio -> Desktop Commander');
console.log(`  node: ${process.version}`);
console.log(`  tunnel-client: ${tunnelVersion}`);
console.log(`  desktop-commander: ${dcPackage.version}`);
console.log(`  ripgrep: ${ripgrep}`);
