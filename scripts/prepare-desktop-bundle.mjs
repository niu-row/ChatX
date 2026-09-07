import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { build } from 'esbuild';

const root = process.cwd();
const resourceDir = path.join(root, 'src-tauri', 'resources');
const backendEntry = path.join(root, 'dist', 'index.js');
const lockPath = path.join(root, 'runtime-lock.json');
if (!fs.existsSync(backendEntry)) {
  throw new Error('dist/index.js not found. Run npm run build before preparing desktop resources.');
}
if (!fs.existsSync(lockPath)) {
  throw new Error('runtime-lock.json is required for desktop packaging.');
}

const runtimeLock = JSON.parse(fs.readFileSync(lockPath, 'utf8'));
const platform = `${os.platform()}-${os.arch()}`;
if (runtimeLock.schemaVersion !== 1 || runtimeLock.platform !== platform) {
  throw new Error(`Runtime lock is for ${runtimeLock.platform ?? 'unknown'}, current platform is ${platform}.`);
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function requireLocked(label, actual, expected) {
  if (actual !== expected) {
    throw new Error(`${label} does not match runtime-lock.json. Expected ${expected}, got ${actual}.`);
  }
}

requireLocked('Node version', process.version, runtimeLock.node.version);
requireLocked('Node SHA-256', sha256(process.execPath), runtimeLock.node.sha256);

fs.rmSync(resourceDir, { recursive: true, force: true });
fs.mkdirSync(resourceDir, { recursive: true });

await build({
  entryPoints: [backendEntry],
  outfile: path.join(resourceDir, 'chatgptx-backend.mjs'),
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node20',
  packages: 'bundle',
  sourcemap: false,
  legalComments: 'none',
  banner: { js: '// ChatX bundled local MCP backend' },
});

if (process.platform !== 'win32') {
  throw new Error('The current desktop installer packaging workflow is configured for Windows.');
}

const nodeTarget = path.join(resourceDir, 'node.exe');
fs.copyFileSync(process.execPath, nodeTarget);

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
if (!tunnelSource) {
  throw new Error('tunnel-client.exe was not found. Install the locked OpenAI tunnel-client or set TUNNEL_CLIENT_PATH.');
}

const tunnelVersionResult = spawnSync(tunnelSource, ['--version'], {
  encoding: 'utf8',
  windowsHide: true,
  timeout: 5000,
});
if (tunnelVersionResult.error || tunnelVersionResult.status !== 0) {
  throw new Error(`Unable to read tunnel-client version: ${tunnelVersionResult.error?.message ?? tunnelVersionResult.stderr}`);
}
const tunnelVersion = (tunnelVersionResult.stdout || tunnelVersionResult.stderr || '').trim();
const tunnelSha = sha256(tunnelSource);
requireLocked('tunnel-client version', tunnelVersion, runtimeLock.tunnelClient.version);
requireLocked('tunnel-client SHA-256', tunnelSha, runtimeLock.tunnelClient.sha256);

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

const manifest = {
  schemaVersion: 1,
  platform,
  runtimePolicy: 'locked',
  node: { version: process.version, sha256: sha256(nodeTarget) },
  tunnelClient: {
    version: tunnelVersion,
    sha256: sha256(tunnelTarget),
    licenseSha256: legalHashes.license,
    noticeSha256: legalHashes.notice,
  },
};
fs.writeFileSync(path.join(resourceDir, 'runtime-manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');

console.log(`desktop resources prepared: ${resourceDir}`);
console.log('  policy: locked');
console.log(`  node: ${process.version} (${manifest.node.sha256})`);
console.log(`  tunnel-client: ${tunnelVersion} (${manifest.tunnelClient.sha256})`);
