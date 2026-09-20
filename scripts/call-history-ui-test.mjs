import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const elements = new Map();
function makeElement() {
  const children = [];
  return {
    value: '', textContent: '', hidden: false, disabled: false, checked: false,
    className: '', children, scrollTop: 0, scrollHeight: 0,
    classList: { toggle() {} }, addEventListener() {},
    append(...items) { children.push(...items); },
  };
}
function element(id) {
  if (!elements.has(id)) elements.set(id, makeElement());
  return elements.get(id);
}

let lastHistoryArgs;
const historyPayload = {
  items: [], total: 12, filteredTotal: 4, invalidEntries: 0,
  toolNames: ['read_file', 'write_file'],
  stats: { successCount: 3, errorCount: 1, averageDurationMs: 12.5, p95DurationMs: 40 },
};
const status = { runtimeState: 'stopped', runtimeActive: false, keyStorage: 'session only' };
const context = vm.createContext({
  document: {
    getElementById: element,
    querySelectorAll: () => [],
    createElement: () => makeElement(),
    activeElement: null,
  },
  window: {
    confirm: () => true,
    __TAURI__: { core: { invoke: async (command, args) => {
      if (command === 'get_status') return status;
      if (command === 'get_call_history') {
        lastHistoryArgs = args;
        return historyPayload;
      }
      if (command === 'clear_call_history') return { cleared: true };
      throw new Error(`Unexpected command: ${command}`);
    } } },
  },
  setInterval() {},
});

vm.runInContext(fs.readFileSync('desktop/app.js', 'utf8'), context);
await vm.runInContext('refresh()', context);

element('callLimit').value = '500';
element('callToolFilter').value = 'read_file';
element('callStatusFilter').value = 'error';
await vm.runInContext('refreshCallHistory()', context);
assert.equal(JSON.stringify(lastHistoryArgs), JSON.stringify({ limit: 500, toolName: 'read_file', status: 'error' }));
assert.equal(element('callStatCount').textContent, '4');
assert.equal(element('callStatSuccess').textContent, '75%');
assert.equal(element('callStatAvg').textContent, '12.5 ms');
assert.equal(element('callStatP95').textContent, '40 ms');
assert.match(element('callHistorySummary').textContent, /当前显示 0 条/);

element('callLimit').value = '1000';
element('callToolFilter').value = '';
element('callStatusFilter').value = '';
await vm.runInContext('refreshCallHistory()', context);
assert.equal(JSON.stringify(lastHistoryArgs), JSON.stringify({ limit: 1000 }));

console.log('call history UI checks passed: selectable limit, filters and aggregate stats');
