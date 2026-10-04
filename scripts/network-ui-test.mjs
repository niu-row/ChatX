import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const elements = new Map();
function makeElement() {
  return {
    value: '', textContent: '', hidden: false, disabled: false, checked: false,
    className: '', dataset: {}, innerHTML: '', scrollTop: 0, scrollHeight: 0,
    classList: { toggle() {} }, addEventListener() {}, append() {},
  };
}
function element(id) {
  if (!elements.has(id)) elements.set(id, makeElement());
  return elements.get(id);
}

const status = {
  runtimeState: 'ready', runtimeActive: true,
  keyStorage: 'session only', logs: [],
};
let networkInfo = {
  proxy: {
    mode: 'direct', url: '', systemUrl: null, effectiveUrl: null,
    systemError: null, effectiveError: null,
  },
  relay: {
    enabled: false,
    baseUrl: 'https://relay.example.com',
    registered: false,
    credentialSaved: false,
    setupProvisioned: true,
    status: {
      enabled: false, configured: false, connected: false,
      connecting: false, desktopId: null, sessionId: null,
      lastHeartbeatAt: null, reconnectAttempt: 0, lastError: '',
    },
  },
  tunnel: {
    health: 'healthy', state: 'ready',
    controlPlaneState: 'healthy', controlPlaneReason: '',
    controlPlaneFailures: 0, proxyMode: 'direct', proxySource: '',
  },
};
let savedPayload;
let testedPayload;
let registrationCalls = 0;
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
      if (command === 'get_power_settings') {
        return { supported: true, clamshellAwake: false, managedByChatX: false };
      }
      if (command === 'set_network_settings') {
        savedPayload = args.payload;
        const enabled = Boolean(args.payload.relay.enabled);
        networkInfo = {
          ...networkInfo,
          proxy: {
            ...networkInfo.proxy, ...args.payload.proxy,
            effectiveUrl: args.payload.proxy.url || null,
          },
          relay: {
            ...networkInfo.relay, ...args.payload.relay,
            registered: false,
            credentialSaved: false,
            status: {
              ...networkInfo.relay.status,
              enabled, configured: false, connected: false,
              desktopId: null, sessionId: null, lastHeartbeatAt: null,
            },
          },
          tunnel: {
            ...networkInfo.tunnel,
            proxyMode: args.payload.proxy.mode,
            proxySource: args.payload.proxy.url || '',
          },
        };
        return networkInfo;
      }
      if (command === 'register_relay_desktop') {
        registrationCalls += 1;
        networkInfo = {
          ...networkInfo,
          relay: {
            ...networkInfo.relay,
            registered: true,
            credentialSaved: true,
            status: {
              ...networkInfo.relay.status,
              configured: true,
              connected: true,
              desktopId: 'd_0123456789abcdef',
              sessionId: 's_0123456789abcdef',
              lastHeartbeatAt: Date.now(),
            },
          },
        };
        return networkInfo;
      }
      if (command === 'test_network_settings') {
        testedPayload = args.payload;
        return {
          proxy: { ok: true, target: '127.0.0.1:7890' },
          relay: { ok: true, target: 'https://relay.example.com', error: null },
        };
      }
      throw new Error(`Unexpected command: ${command}`);
    } } },
  },
  setInterval() {},
});

vm.runInContext(fs.readFileSync('desktop/app.js', 'utf8'), context);
await vm.runInContext('refreshNetworkSettings()', context);
assert.equal(element('proxyMode').value, 'direct');
assert.equal(element('proxyUrl').disabled, true);
assert.match(element('networkStatus').textContent, /DIRECT.*HEALTHY/);
assert.equal(element('controlPlaneFact').textContent, 'HEALTHY');
assert.equal(element('relayStatus').textContent, '已关闭');
element('proxyMode').value = 'manual';
element('proxyUrl').value = 'http://127.0.0.1:7890';
element('relayEnabled').checked = true;
element('relayBaseUrl').value = 'https://relay.example.com';
vm.runInContext('networkDirty = true', context);

await vm.runInContext('saveRelaySettings()', context);
assert.equal(savedPayload.proxy.mode, 'manual');
assert.equal(savedPayload.proxy.url, 'http://127.0.0.1:7890');
assert.equal(savedPayload.relay.enabled, true);
assert.equal(savedPayload.relay.baseUrl, 'https://relay.example.com');
assert.equal('setupToken' in savedPayload.relay, false);
assert.equal(element('relayStatus').textContent, '待注册');
assert.equal(element('relayRegistrationFact').textContent, '未注册 · 可注册');
assert.equal(element('registerRelay').textContent, '注册此电脑');

await vm.runInContext('registerRelayDesktop()', context);
assert.equal(registrationCalls, 1);
assert.equal(element('relayStatus').textContent, '已连接');
assert.equal(element('relayRegistrationFact').textContent, '已注册 · 安全存储');
assert.equal(element('registerRelay').textContent, '已注册');
assert.match(element('relaySessionFact').textContent, /CONNECTED/);

await vm.runInContext('testNetworkSettings()', context);
assert.equal(testedPayload.proxy.mode, 'manual');
assert.match(element('networkTestResult').textContent, /Tunnel OK/);

await vm.runInContext('testRelaySettings()', context);
assert.match(element('relayTestResult').textContent, /Relay OK/);
console.log('network UI checks passed: proxy, relay registration and WSS settings');
