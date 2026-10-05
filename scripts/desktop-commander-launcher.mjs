import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [, , homeArg, entryArg, ...serverArgs] = process.argv;
if (!homeArg || !entryArg) {
  console.error('ChatX Desktop Commander launcher requires <home> <entry> [args...]');
  process.exit(2);
}

const home = path.resolve(homeArg);
const entry = path.resolve(entryArg);
fs.mkdirSync(home, { recursive: true });

// CHATX_TUNNEL_RUNTIME_KEY is only for tunnel-client authentication. The MCP
// process must not retain it because Desktop Commander can launch arbitrary
// child processes that would otherwise inherit the secret.
delete process.env.CHATX_TUNNEL_RUNTIME_KEY;

// Desktop Commander derives its config path from os.homedir(). Keep the bundled
// ChatX instance isolated from any separately installed Desktop Commander.
process.env.HOME = home;
process.env.USERPROFILE = home;
// Hard upstream kill-switch: do not send Desktop Commander telemetry from ChatX.
process.env.DESKTOP_COMMANDER_DISABLE_TELEMETRY = '1';

process.argv = [process.execPath, entry, ...serverArgs];

// Keep the locked upstream package intact. Adapt its registered MCP handlers
// before connecting stdio so ChatX exposes tools without embedded UI resources.
// Upstream's preview A/B flag is not a reliable off switch and still lists UI.
const monitorDir = path.join(home, '.chatx-monitor');
const activityPath = path.join(monitorDir, 'activity.json');
fs.mkdirSync(monitorDir, { recursive: true });
const launcherStartedAt = Date.now();
let activitySequence = 0;
let callSequence = 0;
let lastRequestAt = null;
let lastRequestMethod = null;
let lastListToolsAt = null;
let lastCallStartedAt = null;
let lastCallFinishedAt = null;
let lastToolName = null;
let lastSuccess = null;
let lastDurationMs = null;
let recentCallStarts = [];
const inFlightCalls = new Map();

function writeActivitySnapshot() {
  try {
    const now = Date.now();
    recentCallStarts = recentCallStarts.filter((timestamp) => now - timestamp <= 60_000);
    const inFlightCallList = [...inFlightCalls.entries()].map(([id, call]) => ({
      id,
      toolName: call.toolName,
      startedAt: call.startedAt,
    }));
    const oldestInFlightStartedAt = inFlightCallList.length
      ? Math.min(...inFlightCallList.map((call) => call.startedAt))
      : null;
    const payload = {
      schemaVersion: 3,
      sequence: ++activitySequence,
      updatedAt: now,
      launcherStartedAt,
      lastRequestAt,
      lastRequestMethod,
      lastListToolsAt,
      lastCallStartedAt,
      lastCallFinishedAt,
      lastToolName,
      lastSuccess,
      lastDurationMs,
      inFlight: inFlightCalls.size,
      inFlightCalls: inFlightCallList,
      oldestInFlightStartedAt,
      recentCallStarts,
      callsLastMinute: recentCallStarts.length,
    };
    const temp = `${activityPath}.${process.pid}.tmp`;
    fs.writeFileSync(temp, `${JSON.stringify(payload)}\n`, { mode: 0o600 });
    fs.renameSync(temp, activityPath);
  } catch {
    // Monitoring is observational only. Snapshot I/O must never break an MCP call.
  }
}

function recordRequest(method) {
  const now = Date.now();
  lastRequestAt = now;
  lastRequestMethod = method;
  if (method === 'tools/list') lastListToolsAt = now;
  writeActivitySnapshot();
}

// Clear any stale in-flight snapshot left by a previous crashed launcher.
writeActivitySnapshot();

const dist = path.dirname(entry);
await import(pathToFileURL(path.join(dist, 'bootstrap.js')).href);
const sdk = path.join(dist, '..', 'node_modules', '@modelcontextprotocol', 'sdk', 'dist', 'esm');
const { Server } = await import(pathToFileURL(path.join(sdk, 'server', 'index.js')).href);
const types = await import(pathToFileURL(path.join(sdk, 'types.js')).href);
const originalSetRequestHandler = Server.prototype.setRequestHandler;
Server.prototype.setRequestHandler = function (schema, handler) {
  if (schema === types.ListToolsRequestSchema) {
    const upstream = handler;
    handler = async (...args) => {
      recordRequest('tools/list');
      const result = await upstream(...args);
      return { ...result, tools: result.tools.map((tool) => {
        const meta = { ...tool._meta };
        delete meta.ui;
        delete meta['ui/resourceUri'];
        for (const key of ['openai/outputTemplate', 'openai/widgetAccessible']) delete meta[key];
        const { _meta, ...rest } = tool;
        return Object.keys(meta).length ? { ...rest, _meta: meta } : rest;
      }) };
    };
  } else if (schema === types.CallToolRequestSchema) {
    const upstream = handler;
    handler = async (...args) => {
      const request = args[0];
      const startedAt = Date.now();
      const callId = `${process.pid}:${++callSequence}:${startedAt}`;
      const toolName = request?.params?.name || 'unknown';
      lastRequestAt = startedAt;
      lastRequestMethod = 'tools/call';
      lastCallStartedAt = startedAt;
      lastToolName = toolName;
      recentCallStarts.push(startedAt);
      inFlightCalls.set(callId, { toolName, startedAt });
      writeActivitySnapshot();
      try {
        const result = await upstream(...args);
        lastSuccess = result?.isError !== true;
        return result;
      } catch (error) {
        lastSuccess = false;
        throw error;
      } finally {
        const finishedAt = Date.now();
        lastCallStartedAt = startedAt;
        lastCallFinishedAt = finishedAt;
        lastToolName = toolName;
        lastDurationMs = Math.max(0, finishedAt - startedAt);
        inFlightCalls.delete(callId);
        writeActivitySnapshot();
      }
    };
  } else if (schema === types.ListResourcesRequestSchema) {
    handler = async () => { recordRequest('resources/list'); return { resources: [] }; };
  } else if (schema === types.ListResourceTemplatesRequestSchema) {
    handler = async () => { recordRequest('resources/templates/list'); return { resourceTemplates: [] }; };
  } else if (schema === types.ReadResourceRequestSchema) {
    handler = async () => { recordRequest('resources/read'); throw new types.McpError(-32002, 'ChatX does not expose UI resources.'); };
  }
  return originalSetRequestHandler.call(this, schema, handler);
};
try {
  await import(pathToFileURL(path.join(dist, 'server.js')).href);
} finally {
  Server.prototype.setRequestHandler = originalSetRequestHandler;
}
await import(pathToFileURL(entry).href);
