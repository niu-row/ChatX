import fs from 'node:fs';

const main = fs.readFileSync('src-tauri/src/main.rs', 'utf8');
const html = fs.readFileSync('desktop/index.html', 'utf8');
const app = fs.readFileSync('desktop/app.js', 'utf8');
const prepare = fs.readFileSync('scripts/prepare-desktop-bundle.mjs', 'utf8');
const tauri = fs.readFileSync('src-tauri/tauri.conf.json', 'utf8');
const runtimeLock = JSON.parse(fs.readFileSync('runtime-lock.json', 'utf8'));
const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));

function requireText(label, text, needle) {
  if (!text.includes(needle)) throw new Error(`${label} is missing: ${needle}`);
}
function rejectText(label, text, needle) {
  if (text.includes(needle)) throw new Error(`${label} still contains obsolete runtime text: ${needle}`);
}

requireText('main.rs', main, 'runtimes');
requireText('main.rs', main, 'connect');
requireText('main.rs', main, '--mcp-command');
requireText('main.rs', main, 'desktop-commander');
requireText('main.rs', main, 'env:CHATX_TUNNEL_RUNTIME_KEY');
requireText('main.rs', main, 'ProtectedData');
rejectText('main.rs', main, '127.0.0.1:3210');
rejectText('main.rs', main, 'backend_request');
rejectText('main.rs', main, 'chatgptx-backend.mjs');

requireText('desktop/index.html', html, 'Desktop Commander');
requireText('desktop/index.html', html, 'Secure MCP Tunnel');
requireText('desktop/app.js', app, "invoke('connect_tunnel'");
requireText('desktop/app.js', app, "invoke('get_status'");
rejectText('desktop/app.js', app, '/api/tunnel/status');
rejectText('desktop/app.js', app, '/api/settings');
rejectText('desktop/app.js', app, 'permissionPreset');

requireText('prepare-desktop-bundle.mjs', prepare, '@wonderwhy-er/desktop-commander@');
requireText('prepare-desktop-bundle.mjs', prepare, '--ignore-scripts');
requireText('prepare-desktop-bundle.mjs', prepare, 'DesktopCommander-LICENSE.txt');
rejectText('prepare-desktop-bundle.mjs', prepare, 'chatgptx-backend.mjs');

requireText('tauri.conf.json', tauri, 'resources/desktop-commander');
requireText('tauri.conf.json', tauri, 'DesktopCommander-LICENSE.txt');
rejectText('tauri.conf.json', tauri, 'chatgptx-backend.mjs');

if (runtimeLock.schemaVersion !== 2) throw new Error('runtime-lock.json schemaVersion must be 2.');
if (runtimeLock.desktopCommander?.version !== '0.2.48') throw new Error('Desktop Commander must be locked to 0.2.48.');
if (!pkg.description.includes('Desktop Commander')) throw new Error('package description must describe the new bridge architecture.');
if (pkg.scripts?.test?.includes('security-regression-test')) throw new Error('package test still references the removed MCP backend.');

console.log('desktop static checks passed');
