import fs from 'node:fs';

const main = fs.readFileSync('src-tauri/src/main.rs', 'utf8');
const html = fs.readFileSync('desktop/index.html', 'utf8');
const app = fs.readFileSync('desktop/app.js', 'utf8');
const prepare = fs.readFileSync('scripts/prepare-desktop-bundle.mjs', 'utf8');
const launcher = fs.readFileSync('scripts/desktop-commander-launcher.mjs', 'utf8');
const bridge = fs.readFileSync('scripts/bridge-smoke-test.mjs', 'utf8');
const tauri = fs.readFileSync('src-tauri/tauri.conf.json', 'utf8');
const runtimeLock = JSON.parse(fs.readFileSync('runtime-lock.json', 'utf8'));
const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));

function requireText(label, text, needle) {
  if (!text.includes(needle)) throw new Error(`${label} is missing: ${needle}`);
}
function rejectText(label, text, needle) {
  if (text.includes(needle)) throw new Error(`${label} still contains obsolete runtime text: ${needle}`);
}

requireText('main.rs', main, '--mcp-command');
requireText('main.rs', main, 'runtimeActive');
requireText('main.rs', main, '"ready" | "running"');
requireText('main.rs', main, 'connection');
requireText('main.rs', main, 'tunnelId');
requireText('main.rs', main, 'desktop-commander-home');
requireText('main.rs', main, 'desktop-commander-launcher.mjs');
requireText('main.rs', main, 'env:CHATX_TUNNEL_RUNTIME_KEY');
requireText('main.rs', main, 'ProtectedData');
rejectText('main.rs', main, '127.0.0.1:3210');
rejectText('main.rs', main, 'backend_request');
rejectText('main.rs', main, 'chatgptx-backend.mjs');

requireText('desktop/index.html', html, 'Desktop Commander');
requireText('desktop/index.html', html, 'Secure MCP Tunnel');
requireText('desktop/app.js', app, 'runtimeActive');
requireText('desktop/app.js', app, "'ready'");
requireText('desktop/app.js', app, "invoke('connect_tunnel'");
requireText('desktop/app.js', app, "invoke('get_status'");
rejectText('desktop/app.js', app, '/api/tunnel/status');
rejectText('desktop/app.js', app, 'permissionPreset');

requireText('prepare-desktop-bundle.mjs', prepare, '@wonderwhy-er/desktop-commander@');
requireText('prepare-desktop-bundle.mjs', prepare, '--ignore-scripts');
requireText('prepare-desktop-bundle.mjs', prepare, 'Desktop Commander ripgrep rebuild');
requireText('prepare-desktop-bundle.mjs', prepare, "'@vscode/ripgrep'");
requireText('prepare-desktop-bundle.mjs', prepare, "'rg.exe'");
requireText('prepare-desktop-bundle.mjs', prepare, 'desktop-commander-launcher.mjs');
requireText('prepare-desktop-bundle.mjs', prepare, 'DesktopCommander-LICENSE.txt');
requireText('prepare-desktop-bundle.mjs', prepare, 'npm_execpath');
requireText('prepare-desktop-bundle.mjs', prepare, 'runNpm');
rejectText('prepare-desktop-bundle.mjs', prepare, "run('Desktop Commander install', 'npm.cmd'");
rejectText('prepare-desktop-bundle.mjs', prepare, "run('Desktop Commander ripgrep rebuild', 'npm.cmd'");

requireText('desktop-commander-launcher.mjs', launcher, 'DESKTOP_COMMANDER_DISABLE_TELEMETRY');
requireText('desktop-commander-launcher.mjs', launcher, 'USERPROFILE');
requireText('desktop-commander-launcher.mjs', launcher, 'HOME');

requireText('bridge-smoke-test.mjs', bridge, "from '@modelcontextprotocol/client/stdio'");
requireText('bridge-smoke-test.mjs', bridge, "call('start_search'");
requireText('bridge-smoke-test.mjs', bridge, 'chatx-ripgrep-smoke');

requireText('tauri.conf.json', tauri, 'resources/desktop-commander');
requireText('tauri.conf.json', tauri, 'desktop-commander-launcher.mjs');
rejectText('tauri.conf.json', tauri, 'chatgptx-backend.mjs');

if (runtimeLock.schemaVersion !== 2) throw new Error('runtime-lock.json schemaVersion must be 2.');
if (runtimeLock.desktopCommander?.version !== '0.2.48') throw new Error('Desktop Commander must be locked to 0.2.48.');
if (pkg.version !== '0.3.0') throw new Error('ChatX bridge release must be version 0.3.0.');
if (!pkg.scripts?.['test:bridge']) throw new Error('package scripts must include the real Desktop Commander bridge smoke test.');

console.log('desktop static checks passed');
