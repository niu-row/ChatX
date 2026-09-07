import fs from 'node:fs';

const main = fs.readFileSync('src-tauri/src/main.rs', 'utf8');
const html = fs.readFileSync('desktop/index.html', 'utf8');
const app = fs.readFileSync('desktop/app.js', 'utf8');
const prepare = fs.readFileSync('scripts/prepare-desktop-bundle.mjs', 'utf8');
const installer = fs.readFileSync('scripts/build-installer.mjs', 'utf8');
const installerRuntime = fs.readFileSync('scripts/installer-runtime-test.mjs', 'utf8');
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
requireText('main.rs', main, 'root.join("desktop-commander").join("dist").join("index.js")');
requireText('main.rs', main, 'env:CHATX_TUNNEL_RUNTIME_KEY');
requireText('main.rs', main, 'ProtectedData');
requireText('main.rs', main, 'fn runtime_not_running');
requireText('main.rs', main, '("error".into(), false, None, text)');
requireText('main.rs', main, 'Tunnel 状态返回为空或不是有效 JSON');
requireText('main.rs', main, 'let runtime_manifest = manifest(&paths);');
requireText('main.rs', main, '"tunnelVersion":tunnel_version');
requireText('main.rs', main, 'stop_runtime(&app, None)?;');
rejectText('main.rs', main, 'let _ = stop_runtime(&app, None);');
rejectText('main.rs', main, 'join("@wonderwhy-er").join("desktop-commander")');
rejectText('main.rs', main, '"tunnelVersion":executable_version(&paths.tunnel)');
rejectText('main.rs', main, '127.0.0.1:3210');
rejectText('main.rs', main, 'backend_request');
rejectText('main.rs', main, 'chatgptx-backend.mjs');

requireText('desktop/index.html', html, 'Desktop Commander');
requireText('desktop/index.html', html, 'Secure MCP Tunnel');
requireText('desktop/app.js', app, 'runtimeActive');
requireText('desktop/app.js', app, "'ready'");
requireText('desktop/app.js', app, "invoke('connect_tunnel'");
requireText('desktop/app.js', app, "invoke('get_status'");
requireText('desktop/app.js', app, "['error', 'unavailable'].includes");
rejectText('desktop/app.js', app, '/api/tunnel/status');
rejectText('desktop/app.js', app, 'permissionPreset');

requireText('prepare-desktop-bundle.mjs', prepare, 'runtimeLock.schemaVersion !== 3');
requireText('prepare-desktop-bundle.mjs', prepare, 'DESKTOP_COMMANDER_MCPB_PATH');
requireText('prepare-desktop-bundle.mjs', prepare, 'Desktop Commander MCPB SHA-256');
requireText('prepare-desktop-bundle.mjs', prepare, 'releaseUrl');
requireText('prepare-desktop-bundle.mjs', prepare, 'Expand-Archive');
requireText('prepare-desktop-bundle.mjs', prepare, "const dcRoot = path.join(resourceDir, 'desktop-commander');");
requireText('prepare-desktop-bundle.mjs', prepare, "entry: 'desktop-commander/dist/index.js'");
requireText('prepare-desktop-bundle.mjs', prepare, 'locked-github-release');
requireText('prepare-desktop-bundle.mjs', prepare, 'github-release-mcpb');
requireText('prepare-desktop-bundle.mjs', prepare, 'runtimeKeyStrippedByLauncher');
requireText('prepare-desktop-bundle.mjs', prepare, 'createRequire');
requireText('prepare-desktop-bundle.mjs', prepare, 'rgPath');
requireText('prepare-desktop-bundle.mjs', prepare, 'ripgrepRelative');
requireText('prepare-desktop-bundle.mjs', prepare, 'DesktopCommander-LICENSE.txt');
rejectText('prepare-desktop-bundle.mjs', prepare, "node_modules', '@wonderwhy-er', 'desktop-commander'");
rejectText('prepare-desktop-bundle.mjs', prepare, 'npm_execpath');
rejectText('prepare-desktop-bundle.mjs', prepare, 'runNpm');
rejectText('prepare-desktop-bundle.mjs', prepare, 'Desktop Commander install');
rejectText('prepare-desktop-bundle.mjs', prepare, 'Desktop Commander ripgrep rebuild');

requireText('installer-runtime-test.mjs', installerRuntime, "manifest.schemaVersion !== 3");
requireText('installer-runtime-test.mjs', installerRuntime, "manifest.runtimePolicy !== 'locked-github-release'");
requireText('installer-runtime-test.mjs', installerRuntime, "const dcRoot = path.join(resourceDir, 'desktop-commander');");
requireText('installer-runtime-test.mjs', installerRuntime, "manifest.desktopCommander?.entry !== 'desktop-commander/dist/index.js'");
requireText('installer-runtime-test.mjs', installerRuntime, 'releaseSha256 !== locked.desktopCommander.sha256');
requireText('installer-runtime-test.mjs', installerRuntime, 'runtimeKeyStrippedByLauncher');
rejectText('installer-runtime-test.mjs', installerRuntime, "node_modules', '@wonderwhy-er', 'desktop-commander'");

requireText('build-installer.mjs', installer, "const publish = args.has('--publish');");
requireText('build-installer.mjs', installer, 'const requireSigning = publish ||');
requireText('build-installer.mjs', installer, 'Publishing a ChatX release requires CHATX_WINDOWS_CERT_THUMBPRINT');
requireText('build-installer.mjs', installer, 'if (certificateThumbprint)');
requireText('build-installer.mjs', installer, 'published signed installer');
rejectText('build-installer.mjs', installer, 'published unsigned installer');
rejectText('build-installer.mjs', installer, 'release is unsigned; this is intentional');

requireText('desktop-commander-launcher.mjs', launcher, 'delete process.env.CHATX_TUNNEL_RUNTIME_KEY');
requireText('desktop-commander-launcher.mjs', launcher, 'DESKTOP_COMMANDER_DISABLE_TELEMETRY');
requireText('desktop-commander-launcher.mjs', launcher, 'USERPROFILE');
requireText('desktop-commander-launcher.mjs', launcher, 'HOME');

requireText('bridge-smoke-test.mjs', bridge, "from '@modelcontextprotocol/client/stdio'");
requireText('bridge-smoke-test.mjs', bridge, "path.join(resourceDir, 'desktop-commander', 'dist', 'index.js')");
requireText('bridge-smoke-test.mjs', bridge, "call('start_search'");
requireText('bridge-smoke-test.mjs', bridge, "call('start_process'");
requireText('bridge-smoke-test.mjs', bridge, "transportEnv.CHATX_TUNNEL_RUNTIME_KEY = 'chatx-secret-smoke'");
requireText('bridge-smoke-test.mjs', bridge, 'CHATX_KEY_STRIPPED');
requireText('bridge-smoke-test.mjs', bridge, 'chatx-ripgrep-smoke');
rejectText('bridge-smoke-test.mjs', bridge, "node_modules', '@wonderwhy-er', 'desktop-commander'");

requireText('tauri.conf.json', tauri, 'resources/desktop-commander');
requireText('tauri.conf.json', tauri, 'desktop-commander-launcher.mjs');
rejectText('tauri.conf.json', tauri, 'chatgptx-backend.mjs');

if (runtimeLock.schemaVersion !== 3) throw new Error('runtime-lock.json schemaVersion must be 3.');
if (runtimeLock.desktopCommander?.version !== '0.2.48') throw new Error('Desktop Commander must be locked to 0.2.48.');
if (runtimeLock.desktopCommander?.releaseTag !== 'v0.2.48') throw new Error('Desktop Commander release tag must be locked to v0.2.48.');
if (runtimeLock.desktopCommander?.releaseAsset !== 'desktop-commander-0.2.48.mcpb') throw new Error('Desktop Commander MCPB release asset must be locked.');
if (!/^https:\/\/github\.com\/wonderwhy-er\/DesktopCommanderMCP\/releases\/download\//.test(runtimeLock.desktopCommander?.releaseUrl ?? '')) {
  throw new Error('Desktop Commander release URL must point to the upstream GitHub release.');
}
if (!/^[0-9a-f]{64}$/i.test(runtimeLock.desktopCommander?.sha256 ?? '')) throw new Error('Desktop Commander MCPB SHA-256 must be locked.');
if (pkg.version !== '0.3.0') throw new Error('ChatX bridge release must be version 0.3.0.');
if (!pkg.scripts?.['test:bridge']) throw new Error('package scripts must include the real Desktop Commander bridge smoke test.');

console.log('desktop static checks passed');
