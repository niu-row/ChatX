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
const context = vm.createContext({
  document: { getElementById: element, querySelectorAll: () => [], activeElement: null },
  window: { __TAURI__: { core: { invoke: async (command, args) => {
    if (command === 'get_status') return status;
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
