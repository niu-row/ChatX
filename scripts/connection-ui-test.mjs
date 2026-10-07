import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const elements = new Map();
function element(id) {
  if (!elements.has(id)) elements.set(id, {
    value: '', textContent: '', hidden: false, disabled: false, checked: false,
    classList: { toggle() {} }, addEventListener() {},
  });
  return elements.get(id);
}
let status = { runtimeState: 'stopped', runtimeActive: false, keyStorage: 'session only' };
let connectionError = '启动 Tunnel 失败：authentication failed';
let attempts = 0;
let lastConnection;
let deferStatus = false;
const statusRequests = [];
const context = vm.createContext({
  document: { getElementById: element, querySelectorAll: () => [], activeElement: null },
  window: { __TAURI__: { core: { invoke: async (command, args) => {
    if (command === 'get_status') {
      if (deferStatus) {
        return new Promise((resolve) => statusRequests.push(resolve));
      }
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
