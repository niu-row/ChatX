import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { build } from 'esbuild';

const root = process.cwd();
const resourceDir = path.join(root, 'src-tauri', 'resources');
const backendEntry = path.join(root, 'dist', 'index.js');

if (!fs.existsSync(backendEntry)) {
  throw new Error('dist/index.js not found. Run npm run build before preparing desktop resources.');
}

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
  banner: {
    js: '// ChatX bundled local MCP backend',
  },
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
  throw new Error('tunnel-client.exe was not found. Install OpenAI tunnel-client or set TUNNEL_CLIENT_PATH before packaging.');
}

const tunnelTarget = path.join(resourceDir, 'tunnel-client.exe');
fs.copyFileSync(tunnelSource, tunnelTarget);

for (const name of ['LICENSE', 'NOTICE']) {
  const candidates = [
    path.join(path.dirname(tunnelSource), name),
    path.join(path.dirname(tunnelSource), `tunnel-client-${name}.txt`),
  ];
  const localAppData = process.env.LOCALAPPDATA;
  if (localAppData) {
    candidates.push(path.join(localAppData, 'Programs', 'OpenAI', 'tunnel-client', name));
  }
  const source = candidates.find((candidate) => fs.existsSync(candidate));
  if (!source) {
    throw new Error(`tunnel-client ${name} file was not found next to the selected binary or in the standard install directory.`);
  }
  fs.copyFileSync(source, path.join(resourceDir, `tunnel-client-${name}.txt`));
}

const tunnelVersionResult = spawnSync(tunnelSource, ['--version'], {
  encoding: 'utf8',
  windowsHide: true,
  timeout: 5000,
});
const tunnelVersion = (tunnelVersionResult.stdout || tunnelVersionResult.stderr || '').trim() || null;

fs.writeFileSync(
  path.join(resourceDir, 'runtime-manifest.json'),
  `${JSON.stringify({
    generatedAt: new Date().toISOString(),
    platform: `${os.platform()}-${os.arch()}`,
    node: process.version,
    tunnelClient: tunnelVersion,
  }, null, 2)}\n`,
  'utf8',
);

console.log(`desktop resources prepared: ${resourceDir}`);
console.log(`  node: ${process.version}`);
console.log(`  tunnel-client: ${tunnelVersion ?? 'unknown'}`);
