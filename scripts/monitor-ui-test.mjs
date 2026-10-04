import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const elements = new Map();
function makeElement() {
  return {
    value: '', textContent: '', hidden: false, disabled: false, checked: false,
    className: '', dataset: {}, innerHTML: '', scrollTop: 0, scrollHeight: 0,
    children: [],
    classList: { toggle() {} }, addEventListener() {},
    append(...items) { this.children.push(...items); },
  };
}
function element(id) {
  if (!elements.has(id)) elements.set(id, makeElement());
  return elements.get(id);
}

const status = {
  runtimeState: 'stopped', runtimeActive: false,
  keyStorage: 'session only', logs: [],
};
let monitorInfo = {
  enabled: false, port: 18432, running: false, bindAddress: null,
  fingerprintSha256: null, protocol: 'chatx-monitor-wss-v1',
  encryption: 'AES-256-GCM', devices: [],
};
let networkInfo = {
  proxy: {
    mode: 'direct', url: '', systemUrl: null,
    effectiveUrl: null, systemError: null, effectiveError: null,
  },
  relay: {
    enabled: false, baseUrl: '', credentialSaved: false,
    status: {
      enabled: false, configured: false, connected: false,
      connecting: false, desktopId: null, sessionId: null,
      lastHeartbeatAt: null, reconnectAttempt: 0, lastError: '',
    },
  },
  tunnel: {
    health: 'stopped', state: 'stopped', controlPlaneState: 'unknown',
    controlPlaneReason: '', controlPlaneFailures: 0,
    proxyMode: 'direct', proxySource: '',
  },
};
let lastSetArgs;
let revokedDeviceId;
const context = vm.createContext({
  document: {
    getElementById: element,
    querySelectorAll: () => [],
    activeElement: null,
    createElement: () => makeElement(),
  },
  navigator: { clipboard: { writeText: async () => {} } },
  window: {
    confirm: () => true,
    __TAURI__: { core: { invoke: async (command, args) => {
      if (command === 'get_status') return status;
      if (command === 'get_network_settings') return networkInfo;
      if (command === 'get_monitor_info') return monitorInfo;
      if (command === 'set_monitor_enabled') {
        lastSetArgs = args;
        monitorInfo = {
          ...monitorInfo,
          enabled: args.enabled, port: args.port, running: args.enabled,
          bindAddress: args.enabled ? `0.0.0.0:${args.port} · [::]:${args.port}` : null,
          fingerprintSha256: args.enabled ? 'ab'.repeat(32) : null,
        };
        return monitorInfo;
      }
      if (command === 'create_monitor_pairing') return {
        schemaVersion: 3,
        protocol: 'chatx-monitor-wss-v1',
        scheme: 'wss',
        desktopId: 'd_0123456789abcdef',
        port: monitorInfo.port,
        fingerprintSha256: 'ab'.repeat(32),
        pairingCode: 'cd'.repeat(32),
        directCandidates: [{
          kind: 'lan', family: 'ipv4', interface: 'en0',
          host: '192.168.1.20',
          url: `wss://192.168.1.20:${monitorInfo.port}/v1/ws/pair`,
        }],
        expiresAt: Date.now() + 300000,
        qrSvg: '<svg>chatx-qr</svg>',
      };
      if (command === 'revoke_monitor_device') {
        revokedDeviceId = args.deviceId;
        monitorInfo = {
          ...monitorInfo,
          devices: monitorInfo.devices.filter((item) => item.id !== args.deviceId),
        };
        return monitorInfo;
      }
      throw new Error(`Unexpected command: ${command}`);
    } } },
  },
  setInterval() {},
});

vm.runInContext(fs.readFileSync('desktop/app.js', 'utf8'), context);
await vm.runInContext('refresh()', context);
await vm.runInContext('refreshMonitorPage()', context);
assert.equal(element('monitorStatus').textContent, '已关闭');
assert.equal(element('monitorPort').value, '18432');
element('monitorEnabled').checked = true;
element('monitorPort').value = '19432';
await vm.runInContext('saveMonitorSettings()', context);
assert.deepEqual(
  JSON.parse(JSON.stringify(lastSetArgs)),
  { enabled: true, port: 19432 },
);
assert.equal(element('monitorStatus').textContent, '已运行');
assert.match(element('monitorBind').textContent, /0\.0\.0\.0:19432/);
assert.equal(element('monitorFingerprint').textContent, 'ab'.repeat(32));
assert.match(element('monitorProtocol').textContent, /WSS.*AES-256-GCM/);

await vm.runInContext('createMonitorPairing()', context);
assert.equal(element('monitorPairing').hidden, false);
assert.match(element('monitorPairingPayload').textContent, /chatx-monitor-wss-v1/);
assert.match(element('monitorPairingPayload').textContent, /wss:\/\/192\.168\.1\.20:19432/);
assert.doesNotMatch(element('monitorPairingPayload').textContent, /qrSvg/);
assert.match(element('monitorPairingQr').innerHTML, /chatx-qr/);

monitorInfo = {
  ...monitorInfo,
  devices: [{
    id: 'dev_0123456789abcdef01234567',
    name: 'Pixel',
    createdAt: Date.now() - 5000,
    lastSeenAt: Date.now(),
  }],
};
await vm.runInContext('refreshMonitorPage()', context);
assert.ok(element('monitorDevices').children.length >= 1);
const renderedDeviceRow = element('monitorDevices').children.at(-1);
assert.equal(renderedDeviceRow.children[1].textContent, '删除');
const deleteButton = element('deleteDeviceButton');
await vm.runInContext(
  "deleteMonitorDevice('dev_0123456789abcdef01234567', document.getElementById('deleteDeviceButton'))",
  context,
);
assert.equal(revokedDeviceId, undefined);
assert.equal(deleteButton.textContent, '确认删除');
await vm.runInContext(
  "deleteMonitorDevice('dev_0123456789abcdef01234567', document.getElementById('deleteDeviceButton'))",
  context,
);
assert.equal(revokedDeviceId, 'dev_0123456789abcdef01234567');

element('monitorPort').value = '80';
await vm.runInContext('saveMonitorSettings()', context);
assert.match(element('error').textContent, /1024-65535/);

console.log('monitor UI checks passed: WSS, E2EE pairing and device delete');
