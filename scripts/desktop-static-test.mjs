import fs from 'node:fs';

const main = fs.readFileSync('src-tauri/src/main.rs', 'utf8');
const html = fs.readFileSync('desktop/index.html', 'utf8');
const app = fs.readFileSync('desktop/app.js', 'utf8');
const css = fs.readFileSync('desktop/app.css', 'utf8');
const prepare = fs.readFileSync('scripts/prepare-desktop-bundle.mjs', 'utf8');
const packageBuilder = fs.readFileSync('scripts/build-package.mjs', 'utf8');
const installer = fs.readFileSync('scripts/build-installer.mjs', 'utf8');
const installerRuntime = fs.readFileSync('scripts/installer-runtime-test.mjs', 'utf8');
const launcher = fs.readFileSync('scripts/desktop-commander-launcher.mjs', 'utf8');
const bridge = fs.readFileSync('scripts/bridge-smoke-test.mjs', 'utf8');
const tauri = fs.readFileSync('src-tauri/tauri.conf.json', 'utf8');
const tauriWindows = fs.readFileSync('src-tauri/tauri.windows.conf.json', 'utf8');
const tauriMacos = fs.readFileSync('src-tauri/tauri.macos.conf.json', 'utf8');
const macInfoPlist = fs.readFileSync('src-tauri/Info.plist', 'utf8');
const macRuntimeLock = JSON.parse(fs.readFileSync('runtime-lock.darwin-arm64.json', 'utf8'));
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
requireText('main.rs', main, 'fn start_runtime_connection');
requireText('main.rs', main, 'fn start_reconnect_monitor');
requireText('main.rs', main, 'fn reconnect_delay_secs');
requireText('main.rs', main, 'desired_connected');
requireText('main.rs', main, 'session_runtime_key');
requireText('main.rs', main, 'fn stop_requested');
requireText('main.rs', main, 'fn request_all_permissions');
requireText('main.rs', main, 'fn open_full_disk_access_settings');
requireText('main.rs', main, 'fn get_power_settings');
requireText('main.rs', main, 'fn set_clamshell_awake');
requireText('main.rs', main, 'fn macos_enable_clamshell_with_watchdog');
requireText('main.rs', main, 'clamshell-awake.owner');
requireText('main.rs', main, 'managedByChatX');
requireText('main.rs', main, '/usr/bin/pmset -a disablesleep 1');
requireText('main.rs', main, '/usr/bin/pmset -a disablesleep 0');
requireText('main.rs', main, 'fn get_call_history');
requireText('main.rs', main, 'fn clear_call_history');
requireText('main.rs', main, 'tool-history.jsonl');
rejectText('main.rs', main, 'let _ = stop_runtime(&app, None);');
rejectText('main.rs', main, 'join("@wonderwhy-er").join("desktop-commander")');
rejectText('main.rs', main, '"tunnelVersion":executable_version(&paths.tunnel)');
rejectText('main.rs', main, '127.0.0.1:3210');
rejectText('main.rs', main, 'backend_request');
rejectText('main.rs', main, 'chatgptx-backend.mjs');

requireText('desktop/index.html', html, 'Desktop Commander');
requireText('desktop/index.html', html, 'Secure MCP Tunnel');
requireText('desktop/index.html', html, 'data-page="calls"');
requireText('desktop/index.html', html, 'data-page="permissions"');
requireText('desktop/index.html', html, 'data-page="settings"');
requireText('desktop/index.html', html, 'id="authorizeAllPermissions"');
requireText('desktop/index.html', html, 'id="openFullDiskAccess"');
requireText('desktop/index.html', html, 'id="autoReconnect"');
requireText('desktop/index.html', html, 'id="clamshellAwake"');
requireText('desktop/index.html', html, '合盖后保持运行');
requireText('desktop/index.html', html, '退出或异常终止后会自动恢复');
requireText('desktop/index.html', html, 'id="callHistory"');
requireText('desktop/index.html', html, 'id="callLimit"');
requireText('desktop/index.html', html, 'id="callStatP95"');
requireText('desktop/app.js', app, 'runtimeActive');
requireText('desktop/app.js', app, "invoke('get_call_history'");
requireText('desktop/app.js', app, "invoke('clear_call_history'");
requireText('desktop/app.js', app, "'ready'");
requireText('desktop/app.js', app, "invoke('connect_tunnel'");
requireText('desktop/app.js', app, "invoke('get_status'");
requireText('desktop/app.js', app, "invoke('get_power_settings'");
requireText('desktop/app.js', app, "invoke('set_clamshell_awake'");
requireText('desktop/app.js', app, "invoke('set_auto_reconnect'");
requireText('desktop/app.js', app, "invoke('get_permission_center'");
requireText('desktop/app.js', app, "invoke('request_all_permissions'");
requireText('desktop/app.js', app, "invoke('open_full_disk_access_settings'");
requireText('desktop/app.js', app, "['error', 'unavailable'].includes");
rejectText('desktop/app.js', app, '/api/tunnel/status');
rejectText('desktop/app.js', app, 'permissionPreset');
requireText('desktop/app.css', css, 'html,body{height:100%;overflow:hidden}');
requireText('desktop/app.css', css, '.page[data-page-panel=calls].active{display:flex;flex:1 1 auto;overflow:hidden}');
requireText('desktop/app.css', css, '.page[data-page-panel=calls] .call-history{flex:1 1 auto;min-height:0;overflow:auto;overscroll-behavior:contain}');
requireText('desktop/app.css', css, '.page[data-page-panel=calls] .call-history{display:flex;flex-direction:column;align-items:stretch;gap:10px}');
requireText('desktop/app.css', css, '.page[data-page-panel=calls] .call-record{flex:0 0 auto;min-height:44px}');

requireText('prepare-desktop-bundle.mjs', prepare, "supportedPlatforms = new Set(['win32-x64', 'darwin-arm64'])");
requireText('prepare-desktop-bundle.mjs', prepare, 'runtimeLock.schemaVersion !== 3');
requireText('prepare-desktop-bundle.mjs', prepare, 'DESKTOP_COMMANDER_MCPB_PATH');
requireText('prepare-desktop-bundle.mjs', prepare, 'Desktop Commander MCPB SHA-256');
requireText('prepare-desktop-bundle.mjs', prepare, 'releaseUrl');
requireText('prepare-desktop-bundle.mjs', prepare, 'Expand-Archive');
requireText('prepare-desktop-bundle.mjs', prepare, "'/usr/bin/unzip'");
requireText('prepare-desktop-bundle.mjs', prepare, "'/usr/bin/tar'");
requireText('prepare-desktop-bundle.mjs', prepare, 'node.archiveSha256');
requireText('prepare-desktop-bundle.mjs', prepare, 'tunnel.releaseAsset');
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

requireText('build-package.mjs', packageBuilder, "MAC_BUNDLE_ID = 'com.chatgptx.local'");
requireText('build-package.mjs', packageBuilder, "LOCAL_SIGNING_PATH = path.resolve('.chatx-local-signing.json')");
requireText('build-package.mjs', packageBuilder, 'loadLocalMacSigning');
requireText('build-package.mjs', packageBuilder, 'certificateSha1');
requireText('build-package.mjs', packageBuilder, 'signingIdentity: localSigning.identity');
requireText('build-package.mjs', packageBuilder, "codesign identifier mismatch");
requireText('build-package.mjs', packageBuilder, 'NSLocalNetworkUsageDescription');
requireText('build-package.mjs', packageBuilder, 'verifyMacBundle(appPath, localSigning)');

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

requireText('tauri.windows.conf.json', tauriWindows, 'resources/desktop-commander');
requireText('tauri.windows.conf.json', tauriWindows, 'resources/node.exe');
requireText('tauri.macos.conf.json', tauriMacos, 'resources/desktop-commander');
requireText('tauri.macos.conf.json', tauriMacos, 'resources/node');
requireText('tauri.macos.conf.json', tauriMacos, 'resources/cloudflared');
requireText('tauri.macos.conf.json', tauriMacos, '"signingIdentity": "-"');
requireText('Info.plist', macInfoPlist, 'NSLocalNetworkUsageDescription');
requireText('Info.plist', macInfoPlist, 'ChatX 需要访问本地网络');
rejectText('tauri.conf.json', tauri, 'chatgptx-backend.mjs');
if (macRuntimeLock.platform !== 'darwin-arm64') throw new Error('macOS runtime lock must target darwin-arm64.');
if (!/^[0-9a-f]{64}$/i.test(macRuntimeLock.node?.archiveSha256 ?? '')) throw new Error('macOS Node archive SHA-256 must be locked.');
if (!/^[0-9a-f]{64}$/i.test(macRuntimeLock.tunnelClient?.sha256 ?? '')) throw new Error('macOS tunnel-client archive SHA-256 must be locked.');

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
