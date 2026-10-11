import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const elements = new Map();
const listeners = new Map();
const settingsTabs = ['connection', 'security', 'network', 'relay', 'system'].map((tab) => ({
  dataset: { settingsTab: tab },
  classList: { toggle() {} },
  setAttribute(name, value) { if (name === 'aria-selected') this.selected = value; },
  addEventListener(event, callback) { listeners.set('settings:' + tab + ':' + event, callback); },
}));
const settingsPanels = ['connection', 'security', 'network', 'relay', 'system'].map((tab) => ({
  dataset: { settingsPanel: tab }, hidden: tab !== 'connection',
}));
function element(id) {
  if (!elements.has(id)) elements.set(id, {
    value: '', textContent: '', hidden: false, disabled: false, checked: false,
    innerHTML: '', children: [],
    classList: { toggle() {} },
    addEventListener(event, callback) { listeners.set(id + ':' + event, callback); },
    appendChild(option) { this.children.push(option); if (!this.value) this.value = option.value; },
  });
  return elements.get(id);
}
let status = { runtimeState: 'stopped', runtimeActive: false, keyStorage: 'session only' };
let connectionError = '启动 Tunnel 失败：authentication failed';
let attempts = 0;
let lastConnection;
let deferStatus = false;
const statusRequests = [];
let authorizationHours = 168;
let httpsAutoEnabled = true;
let grantsRead = 0;
let httpsStarts = 0;
let lastHttpsStart;
let stoppedMarker = false;
let lastAuthorizationUpdate;
let lastRevocation;
let passwordRotations = 0;
let stopAllCalls = 0;
const authorizationRows = [{ id: 'a'.repeat(64), clientTag: 'fixture',
  issuedAt: Math.floor(Date.now() / 1000), expiresAt: null, revoked: false }];
const context = vm.createContext({
  document: { getElementById: element,
    querySelectorAll: (selector) => selector === '[data-settings-tab]' ? settingsTabs
      : selector === '[data-settings-panel]' ? settingsPanels : [],
    activeElement: null,
    createElement: () => ({ value: '', textContent: '', selected: false }) },
  window: { confirm: () => true, __TAURI__: { core: { invoke: async (command, args) => {
    if (command === 'get_status') {
      if (deferStatus) {
        return new Promise((resolve) => statusRequests.push(resolve));
      }
      return status;
    }
    if (command === 'get_https_mcp_status') {
      return { enabled: !stoppedMarker, manualStopBlocked: stoppedMarker,
        baseUrl: 'https://newrow.fun',
        sshHost: '198.51.100.42', sshUser: 'chatx-mcp',
        sshPort: 22, localPort: 3000, remotePort: 18080,
        localReady: true, sshConnected: true, authorizationTtlHours: authorizationHours,
        autoReconnect: httpsAutoEnabled };
    }
    if (command === 'get_https_mcp_authorizations') {
      grantsRead++;
      return { grants: authorizationRows };
    }
    if (command === 'set_https_mcp_auto_reconnect') {
      httpsAutoEnabled = args.enabled;
      return { enabled: !stoppedMarker, autoReconnect: httpsAutoEnabled,
        authorizationTtlHours: authorizationHours };
    }
    if (command === 'start_https_mcp') {
      lastHttpsStart = args.config;
      httpsStarts++;
      return { enabled: true, autoReconnect: httpsAutoEnabled,
        authorizationTtlHours: authorizationHours };
    }
    if (command === 'set_https_mcp_authorization_lifetime') {
      lastAuthorizationUpdate = args.ttlHours;
      authorizationHours = args.ttlHours;
      return { authorizationTtlHours: authorizationHours };
    }
    if (command === 'revoke_https_mcp_authorization') {
      lastRevocation = args.grantId;
      authorizationRows[0].revoked = true;
      return { grants: authorizationRows };
    }
    if (command === 'rotate_https_mcp_login_password') {
      passwordRotations++;
      return 'rotated-oauth-password';
    }
    if (command === 'revoke_all_https_mcp_authorizations') {
      lastRevocation = 'all';
      for (const row of authorizationRows) row.revoked = true;
      return { grants: authorizationRows };
    }
    if (command === 'stop_all_connections') {
      stopAllCalls++;
      status = { ...status, desiredConnected: false, runtimeActive: false,
        httpsMcp: { enabled: false } };
      return status;
    }
    if (command === 'connect_tunnel') {
      attempts++;
      lastConnection = args;
      if (connectionError) throw connectionError;
      status = { ...status, runtimeState: 'ready', runtimeActive: true };
      return status;
    }
    throw new Error(`Unexpected command: ${command}`);
  } } } },
  setInterval() {},
});
vm.runInContext(fs.readFileSync('desktop/api.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/state.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/pages/calls.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/pages/network.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/pages/https-mcp.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/pages/permissions.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/pages/power.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/pages/monitor.js', 'utf8'), context);
vm.runInContext(fs.readFileSync('desktop/app.js', 'utf8'), context);
await vm.runInContext('refresh()', context);
element('tunnelId').value = 'tunnel_test';
await vm.runInContext('connect();', context);
assert.equal(attempts, 0, 'Missing key should produce a useful validation error');
await vm.runInContext('refresh()', context);
assert.match(element('error').textContent, /请输入 Runtime API Key/);
element('runtimeKey').value = 'test-key';
await vm.runInContext('connect()', context);
assert.equal(attempts, 1);
assert.equal(element('error').textContent, connectionError);
await vm.runInContext('refresh()', context);
assert.equal(element('error').textContent, connectionError, 'Polling must preserve connection errors');
assert.equal(element('runtimeKey').value, 'test-key', 'Failed connection must retain entered key');
assert.equal(element('connect').disabled, false, 'Retry should remain available');
connectionError = '';
await vm.runInContext('connect()', context);
assert.equal(element('error').hidden, true, 'Successful retry should clear the error');
assert.equal(element('runtimeKey').value, '');
assert.equal(element('connect').disabled, true);
assert.equal(element('sidebarStatus').textContent, '已连接 ChatGPT');
console.log('connection UI checks passed: validation, error persistence, retry and success');

status = { runtimeState: 'stopped', runtimeActive: false, configured: true, runtimeKeySaved: true, rememberKey: true, keyStorage: 'macOS Keychain' };
await vm.runInContext('refresh()', context);
assert.equal(element('rememberKey').disabled, false);
assert.equal(element('rememberKey').checked, true);
assert.match(element('rememberKeyText').textContent, /macOS 钥匙串/);
assert.equal(element('clearKey').disabled, false);
element('rememberKey').checked = false;
vm.runInContext('rememberKeyDirty = true', context);
await vm.runInContext('refresh()', context);
assert.equal(element('rememberKey').checked, false, 'Polling must not overwrite checkbox edits');
element('rememberKey').checked = true;
await vm.runInContext('connect()', context);
assert.equal(lastConnection.runtimeKey, '', 'Saved key should allow reconnecting without plaintext in the UI');
assert.equal(lastConnection.rememberKey, true);
assert.equal(element('sidebarStatus').textContent, '已连接 ChatGPT');
console.log('keychain UI checks passed: saved-key reconnect and checkbox persistence');

status = {
  ...status,
  runtimeState: 'stopped',
  runtimeActive: false,
  autoReconnect: true,
  desiredConnected: true,
  reconnecting: true,
  reconnectAttempt: 2,
};
await vm.runInContext('refresh()', context);
assert.match(element('sidebarStatus').textContent, /自动重连/);
assert.equal(element('connect').disabled, true, 'Manual connect should be disabled while automatic reconnect is active');
assert.equal(element('stop').disabled, false, 'Stop must remain available to cancel automatic reconnect');
assert.match(element('reconnectStatus').textContent, /第 2 次/);
console.log('automatic reconnect UI checks passed: visible state and cancel path');

status = {
  runtimeState: 'running',
  runtimeActive: false,
  configured: true,
  desiredConnected: true,
  reconnecting: false,
  keyStorage: 'session only',
};
await vm.runInContext('refresh()', context);
assert.notEqual(element('sidebarStatus').textContent, '已连接 ChatGPT', 'Explicit runtimeActive=false must override a running-looking runtimeState');
assert.notEqual(element('tunnelFact').textContent, 'Ready', 'Not-ready runtime must not render as Ready');
console.log('connection UI checks passed: runtimeActive is authoritative');

status = {
  ...status,
  runtimeState: 'ready',
  runtimeActive: false,
  runtimeHealth: 'suspect',
  configured: true,
  desiredConnected: true,
  reconnecting: false,
};
await vm.runInContext('refresh()', context);
assert.match(element('sidebarStatus').textContent, /检查中/);
assert.equal(element('tunnelFact').textContent, 'Suspect');
assert.match(element('heroTitle').textContent, /检查/);

status = {
  ...status,
  runtimeHealth: 'down',
};
await vm.runInContext('refresh()', context);
assert.match(element('sidebarStatus').textContent, /已断开/);
assert.equal(element('tunnelFact').textContent, 'Down');

status = {
  ...status,
  reconnectPhase: 'backoff',
  reconnectAttempt: 3,
  nextReconnectAt: Date.now() + 10_000,
  lastReconnectError: 'temporary control-plane failure',
};
await vm.runInContext('refresh()', context);
assert.match(element('sidebarStatus').textContent, /等待自动重连/);
assert.equal(element('tunnelFact').textContent, 'Backoff');
assert.match(element('reconnectStatus').textContent, /等待自动重连/);
assert.match(element('heroCopy').textContent, /temporary control-plane failure/);
console.log('connection UI checks passed: suspect/down and reconnect backoff are distinct');

deferStatus = true;
const staleRefresh = vm.runInContext('refresh()', context);
const newestRefresh = vm.runInContext('refresh()', context);
statusRequests[1]({
  runtimeState: 'ready',
  runtimeActive: true,
  runtimeHealth: 'healthy',
  keyStorage: 'session only',
  logs: [],
});
await newestRefresh;
statusRequests[0]({
  runtimeState: 'stopped',
  runtimeActive: false,
  runtimeHealth: 'down',
  keyStorage: 'session only',
  logs: [],
});
await staleRefresh;
assert.equal(
  element('sidebarStatus').textContent,
  '已连接 ChatGPT',
  'A stale status response must not overwrite a newer refresh',
);
console.log('connection UI checks passed: stale polling responses are ignored');

await vm.runInContext('httpsMcpPage.refresh()', context);
assert.equal(element('httpsMcpAuthorizationDuration').value, '168');
stoppedMarker = true;
await vm.runInContext('httpsMcpPage.refresh()', context);
assert.match(element('httpsMcpHint').textContent, /上次手动停止标记/);
assert.equal(element('httpsMcpStatus').textContent, '已手动停止');
stoppedMarker = false;
await vm.runInContext('httpsMcpPage.refresh()', context);
element('httpsMcpAuthorizationDuration').value = 'unlimited';
listeners.get('httpsMcpAuthorizationDuration:change')();
assert.equal(element('httpsMcpUnlimitedWarning').hidden, false);
await listeners.get('saveHttpsMcpAuthorizationDuration:click')();
assert.equal(lastAuthorizationUpdate, null, 'Unlimited authorization must serialize as null');
await vm.runInContext('httpsMcpPage.refresh()', context);
assert.equal(element('httpsMcpAuthorizationDuration').value, 'unlimited');
await vm.runInContext('httpsMcpPage.refreshSecurity()', context);
const authorizationSelect = element('httpsMcpAuthorizationList');
authorizationSelect.value = authorizationRows[0].id;
await listeners.get('revokeHttpsMcpAuthorization:click')();
assert.equal(lastRevocation, authorizationRows[0].id);
assert.equal(authorizationRows[0].revoked, true);
await listeners.get('rotateHttpsPassword:click')();
assert.equal(passwordRotations, 1);
assert.equal(element('httpsMcpPassword').value, 'rotated-oauth-password');
assert.equal(element('httpsMcpPassword').type, 'text');
console.log('HTTPS OAuth UI checks passed: unlimited lifetime, per-grant revocation, password rotation');

deferStatus = false;
status = { ...status, runtimeState: 'ready', runtimeActive: true, desiredConnected: true,
  httpsMcp: { enabled: true, localReady: true, sshConnected: true }, keyStorage: 'session only' };
await vm.runInContext('refresh()', context);
await vm.runInContext('stop()', context);
assert.equal(stopAllCalls, 1, 'Global stop must atomically stop both transports in one backend command');
assert.equal(status.httpsMcp.enabled, false);
assert.equal(status.desiredConnected, false);
console.log('Global stop checks passed: both transports stop');

status = { ...status, trayStopError: '托盘停止失败：无法保存停止状态' };
await vm.runInContext('refresh()', context);
assert.match(element('error').textContent, /托盘停止失败/,
  'A tray error must remain visible after the main window reloads');
status = { ...status, trayStopError: null };
await vm.runInContext('refresh()', context);
console.log('Tray stop error survives window reconstruction in status payload');

status = {
  ...status, activeMcpMode: 'https', desiredConnected: false, runtimeActive: false,
  httpsMcp: { enabled: true, localReady: true, sshConnected: true, remoteReady: true }
};
await vm.runInContext('refresh()', context);
assert.equal(element('activeConnectionMode').textContent, '当前：HTTPS MCP');
assert.equal(element('httpsConnectionPanel').hidden, false);
assert.equal(element('tunnelConnectionPanel').hidden, true);
const connectAttemptsBeforeSwitch = attempts;
const stopCallsBeforeSwitch = stopAllCalls;
vm.runInContext("showConnectionMode('tunnel', true)", context);
assert.equal(element('httpsConnectionPanel').hidden, true);
assert.equal(element('tunnelConnectionPanel').hidden, false);
assert.equal(attempts, connectAttemptsBeforeSwitch,
  'Selecting a transport view must not start Tunnel');
assert.equal(stopAllCalls, stopCallsBeforeSwitch,
  'Selecting a transport view must not stop any running connection');
await vm.runInContext('refresh()', context);
assert.equal(element('tunnelConnectionPanel').hidden, false,
  'Status polling must not override the user-selected connection view');
console.log('Mutually exclusive mode UI checks passed: selection is safe and inert');

const markup = fs.readFileSync('desktop/index.html', 'utf8');
const securityLocation = markup.indexOf('data-settings-panel="security"');
const grantsLocation = markup.indexOf('id="httpsMcpAuthorizationList"');
assert.ok(securityLocation > 0 && grantsLocation > securityLocation,
  'OAuth grants must live under Settings > Security');
assert.ok(markup.indexOf('id="revealHttpsPassword"') > securityLocation,
  'OAuth password visibility controls must live in Settings');
assert.equal(markup.includes('data-page-panel="permissions"'), false,
  'The permission center should no longer be a top-level destination');
assert.equal(markup.includes('data-page-panel="logs"'), false,
  'Runtime logs must be integrated into Diagnostics');

const beforeGrantReads = grantsRead;
await vm.runInContext('httpsMcpPage.refresh()', context);
assert.equal(grantsRead, beforeGrantReads,
  'Connection polling must not fetch the OAuth grant registry');

await listeners.get('openHttpsSecurity:click')();
await vm.runInContext('refreshSettingsPage()', context);
assert.equal(vm.runInContext('currentPage', context), 'settings');
assert.equal(vm.runInContext('currentSettingsTab', context), 'security');
assert.ok(grantsRead > beforeGrantReads,
  'Opening Settings > Security must refresh OAuth authorizations');
assert.equal(settingsPanels.find((p) => p.dataset.settingsPanel === 'security').hidden, false);
assert.equal(settingsPanels.find((p) => p.dataset.settingsPanel === 'connection').hidden, true);

element('httpsMcpPassword').value = 'visible-only-in-security';
element('httpsMcpPassword').type = 'text';
vm.runInContext("setSettingsTab('network')", context);
assert.equal(element('httpsMcpPassword').value, '',
  'Navigating away from Security must clear the revealed password');
assert.equal(element('httpsMcpPassword').type, 'password');

vm.runInContext("setSettingsTab('security')", context);
await vm.runInContext('httpsMcpPage.refreshSecurity()', context);
const previouslySavedHours = authorizationHours;
element('httpsMcpAuthorizationDuration').value = previouslySavedHours === null ? '24' : 'unlimited';
listeners.get('httpsMcpAuthorizationDuration:change')();
const startsBefore = httpsStarts;
await listeners.get('startHttpsMcp:click')();
assert.equal(httpsStarts, startsBefore + 1);
assert.equal(lastHttpsStart.authorizationTtlHours, previouslySavedHours,
  'Starting HTTPS must use saved OAuth lifetime, never the unsaved selection');
assert.equal(authorizationHours, previouslySavedHours,
  'Starting HTTPS must not save an unsaved OAuth lifetime edit');

element('httpsAutoReconnect').checked = false;
await listeners.get('httpsAutoReconnect:change')();
assert.equal(httpsAutoEnabled, false,
  'HTTPS auto-reconnect switch must persist separately from Tunnel');

console.log('Settings reorganization passed: OAuth ownership, scoped refresh, independent policy, secret cleanup');
