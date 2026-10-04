import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-monitor-test-'));
const home = path.join(root, 'home');
const dcRoot = path.join(root, 'desktop-commander');
const dist = path.join(dcRoot, 'dist');
const sdk = path.join(dcRoot, 'node_modules', '@modelcontextprotocol', 'sdk', 'dist', 'esm');
const serverDir = path.join(sdk, 'server');
fs.mkdirSync(dist, { recursive: true });
fs.mkdirSync(serverDir, { recursive: true });
fs.writeFileSync(path.join(dcRoot, 'package.json'), '{"type":"module"}\n');

fs.writeFileSync(path.join(dist, 'bootstrap.js'), '');
fs.writeFileSync(path.join(dist, 'index.js'), '');
fs.writeFileSync(path.join(sdk, 'types.js'), `
export const ListToolsRequestSchema = {};
export const CallToolRequestSchema = {};
export const ListResourcesRequestSchema = {};
export const ListResourceTemplatesRequestSchema = {};
export const ReadResourceRequestSchema = {};
export class McpError extends Error {}
`);
fs.writeFileSync(path.join(serverDir, 'index.js'), `
export class Server {
  setRequestHandler(schema, handler) {
    globalThis.__chatxHandlers ??= new Map();
    globalThis.__chatxHandlers.set(schema, handler);
    return this;
  }
}
`);

fs.writeFileSync(path.join(dist, 'server.js'), `
import fs from 'node:fs';
import path from 'node:path';
import { Server } from '../node_modules/@modelcontextprotocol/sdk/dist/esm/server/index.js';
import * as types from '../node_modules/@modelcontextprotocol/sdk/dist/esm/types.js';

const initialActivityFile = path.join(process.env.HOME, '.chatx-monitor', 'activity.json');
const initialActivity = JSON.parse(fs.readFileSync(initialActivityFile, 'utf8'));
if (initialActivity.inFlight !== 0 || initialActivity.inFlightCalls.length !== 0) {
  throw new Error('launcher did not clear stale in-flight activity on startup');
}

const server = new Server();
server.setRequestHandler(types.CallToolRequestSchema, async (request) => {
  const file = path.join(process.env.HOME, '.chatx-monitor', 'activity.json');
  const active = JSON.parse(fs.readFileSync(file, 'utf8'));
  if (active.inFlight !== 1) throw new Error('expected inFlight=1 while handler runs');
  return { isError: request.params.name === 'error' };
});
for (const name of ['read_file', 'write_file', 'error']) {
  const handler = globalThis.__chatxHandlers.get(types.CallToolRequestSchema);
  await handler({ params: { name, arguments: { secret: 'must-not-be-stored' } } });
}
`);
const staleMonitorDir = path.join(home, '.chatx-monitor');
fs.mkdirSync(staleMonitorDir, { recursive: true });
fs.writeFileSync(path.join(staleMonitorDir, 'activity.json'), JSON.stringify({
  schemaVersion: 2,
  sequence: 99,
  updatedAt: Date.now() - 60_000,
  lastCallStartedAt: Date.now() - 60_000,
  lastCallFinishedAt: null,
  lastToolName: 'stale_tool',
  lastSuccess: null,
  lastDurationMs: null,
  inFlight: 1,
  inFlightCalls: [{ id: 'stale', toolName: 'stale_tool', startedAt: Date.now() - 60_000 }],
  oldestInFlightStartedAt: Date.now() - 60_000,
  recentCallStarts: [],
  callsLastMinute: 0,
}) + '\n');

const launcher = path.resolve('scripts', 'desktop-commander-launcher.mjs');
const result = spawnSync(process.execPath, [launcher, home, path.join(dist, 'index.js')], {
  cwd: path.resolve('.'),
  encoding: 'utf8',
});

try {
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const activityPath = path.join(home, '.chatx-monitor', 'activity.json');
  const text = fs.readFileSync(activityPath, 'utf8');
  const activity = JSON.parse(text);
  assert.equal(activity.schemaVersion, 2);
  assert.equal(activity.inFlight, 0);
  assert.deepEqual(activity.inFlightCalls, []);
  assert.equal(activity.lastToolName, 'error');
  assert.equal(activity.lastSuccess, false);
  assert.equal(activity.callsLastMinute, 3);
  assert.equal(activity.recentCallStarts.length, 3);
  assert.ok(activity.lastCallStartedAt > 0);
  assert.ok(activity.lastCallFinishedAt >= activity.lastCallStartedAt);
  assert.doesNotMatch(text, /must-not-be-stored/);
  assert.doesNotMatch(text, /arguments/);
  console.log('monitor instrumentation checks passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
