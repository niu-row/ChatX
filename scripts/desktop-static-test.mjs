import assert from 'node:assert/strict';
import fs from 'node:fs';

const read = (path) => fs.readFileSync(path, 'utf8');
const json = (path) => JSON.parse(read(path));

const main = read('src-tauri/src/main.rs');
const connectionCommands = read('src-tauri/src/connection_commands.rs');
const networkCommands = read('src-tauri/src/network_commands.rs');
const settingsSource = read('src-tauri/src/settings.rs');
const tunnelRuntime = read('src-tauri/src/tunnel_runtime.rs');
const httpsMcp = read('src-tauri/src/https_mcp.rs');
const launcher = read('scripts/desktop-commander-launcher.mjs');
const installer = read('scripts/build-installer.mjs');
const tauri = json('src-tauri/tauri.conf.json');
const windows = json('src-tauri/tauri.windows.conf.json');
const macos = json('src-tauri/tauri.macos.conf.json');
const winLock = json('runtime-lock.json');
const macLock = json('runtime-lock.darwin-arm64.json');

function assertResources(config, expected) {
  const resources = config.bundle?.resources ?? {};
  for (const source of expected) {
    assert.ok(source in resources, `Missing bundled resource: ${source}`);
  }
}

assert.equal(tauri.identifier, 'com.chatgptx.local');
assert.equal(tauri.build?.frontendDist, '../desktop');
assert.doesNotMatch(JSON.stringify(tauri), /chatgptx-backend\.mjs/);
assertResources(windows, [
  'resources/node.exe',
  'resources/tunnel-client.exe',
  'resources/desktop-commander-launcher.mjs',
  'resources/https-mcp-bridge.mjs',
  'resources/https-mcp-parent-watch.mjs',
  'resources/runtime-manifest.json',
  'resources/DesktopCommander-LICENSE.txt',
  'resources/desktop-commander',
]);
assertResources(macos, [
  'resources/node',
  'resources/tunnel-client',
  'resources/cloudflared',
  'resources/cloudflared-manifest.json',
  'resources/desktop-commander-launcher.mjs',
  'resources/https-mcp-bridge.mjs',
  'resources/https-mcp-parent-watch.mjs',
  'resources/runtime-manifest.json',
  'resources/DesktopCommander-LICENSE.txt',
  'resources/desktop-commander',
]);

assert.equal(winLock.schemaVersion, 3);
assert.equal(winLock.platform, 'win32-x64');
assert.equal(macLock.schemaVersion, 3);
assert.equal(macLock.platform, 'darwin-arm64');
assert.equal(winLock.desktopCommander.sha256, macLock.desktopCommander.sha256);
assert.equal(winLock.desktopCommander.releaseTag, macLock.desktopCommander.releaseTag);
for (const lock of [winLock, macLock]) {
  assert.match(lock.desktopCommander.sha256, /^[a-f0-9]{64}$/);
  assert.match(lock.tunnelClient.sha256, /^[a-f0-9]{64}$/);
}

assert.match(tunnelRuntime, /env:CHATX_TUNNEL_RUNTIME_KEY/);
assert.doesNotMatch(tunnelRuntime, /--runtime-api-key\s+\{?runtime/i);
assert.match(launcher, /delete process\.env\.CHATX_TUNNEL_RUNTIME_KEY/);
assert.match(launcher, /DESKTOP_COMMANDER_DISABLE_TELEMETRY/);

// Windows child startup must stay silent and keep the primary thread
// suspended until the job assignment is complete.
const windowsJob = read('src-tauri/src/windows_process_job.rs');
assert.match(windowsJob, /CREATE_NO_WINDOW\s*\|\s*CREATE_SUSPENDED/);
assert.match(httpsMcp, /KillOnCloseJob::spawn\(command\)/);

const getStatus = connectionCommands.match(/fn get_status\b[\s\S]*?\n}/)?.[0] ?? '';
assert.ok(getStatus, 'get_status must remain defined');
assert.match(getStatus, /runtime_snapshot\.lock\(\)/);
assert.doesNotMatch(getStatus, /runtime_status\(&paths\)/);

assert.match(settingsSource, /fn load_settings_checked/);
assert.match(tunnelRuntime, /load_settings_checked\(app\)/);
assert.match(tunnelRuntime, /observation\.runtime_alive/);
assert.match(networkCommands, /fn rollback_network_settings/);
assert.match(
  networkCommands,
  /proxy_changed && wants_connection,[\s\S]*?true,[\s\S]*?rollback_suffix/,
);
assert.doesNotMatch(connectionCommands, /let _ = stop_runtime/);

assert.match(installer, /Publishing a ChatX release requires CHATX_WINDOWS_CERT_THUMBPRINT/);
assert.doesNotMatch(installer, /published unsigned installer/);

console.log('desktop static invariants passed');
