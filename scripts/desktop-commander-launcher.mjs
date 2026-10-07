import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import {
  resolveRealAccountHome,
  restoreSafeHostEnvironment,
} from './desktop-host-environment.mjs';

const [, , homeArg, entryArg, ...serverArgs] = process.argv;
if (!homeArg || !entryArg) {
  console.error('ChatX Desktop Commander launcher requires <home> <entry> [args...]');
  process.exit(2);
}

// A stdio MCP server has no useful lifetime after its parent closes stdin.
// Desktop Commander keeps timers/sockets alive, so without an explicit guard a
// killed/restarted tunnel-client can leave this Node process orphaned for hours.
let stdioDisconnected = false;
function exitOnStdioDisconnect() {
  if (stdioDisconnected) return;
  stdioDisconnected = true;
  process.exitCode = 0;
  // Give the MCP stdio transport a brief chance to close its libuv handles
  // cleanly. Desktop Commander keeps other referenced timers/sockets alive, so
  // force exit after the grace period if natural shutdown does not complete.
  const forceExit = setTimeout(() => process.exit(0), 750);
  forceExit.unref();
}
process.stdin.once('end', exitOnStdioDisconnect);
process.stdin.once('close', exitOnStdioDisconnect);

const home = path.resolve(homeArg);
const entry = path.resolve(entryArg);
fs.mkdirSync(home, { recursive: true });

// Resolve the real login-account home before replacing HOME/USERPROFILE for the
// bundled Desktop Commander process. MCP stdio intentionally filters the parent
// environment, so restore only deterministic, non-secret host values here.
const realHome = resolveRealAccountHome();
restoreSafeHostEnvironment({ realHome });

// CHATX_TUNNEL_RUNTIME_KEY is only for tunnel-client authentication. The MCP
// process must not retain it because Desktop Commander can launch arbitrary
// child processes that would otherwise inherit the secret.
delete process.env.CHATX_TUNNEL_RUNTIME_KEY;

// Desktop Commander derives its config path from os.homedir(). Keep the bundled
// ChatX instance isolated from any separately installed Desktop Commander.
process.env.HOME = home;
process.env.USERPROFILE = home;
process.env.CHATX_REAL_HOME = realHome;

const dist = path.dirname(entry);
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
let activityWriteTimer = null;
let activityWriteInFlight = false;
let activityWriteDirty = false;

function buildActivitySnapshot() {
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
  return {
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
}

function writeActivitySnapshotSync() {
  try {
    const payload = buildActivitySnapshot();
    const temp = `${activityPath}.${process.pid}.tmp`;
    fs.writeFileSync(temp, `${JSON.stringify(payload)}\n`, { mode: 0o600 });
    fs.renameSync(temp, activityPath);
  } catch {
    // Monitoring is observational only. Snapshot I/O must never break an MCP call.
  }
}

async function flushActivitySnapshot() {
  if (activityWriteInFlight || !activityWriteDirty) return;
  activityWriteDirty = false;
  activityWriteInFlight = true;
  try {
    const payload = buildActivitySnapshot();
    const temp = `${activityPath}.${process.pid}.tmp`;
    await fs.promises.writeFile(temp, `${JSON.stringify(payload)}\n`, { mode: 0o600 });
    await fs.promises.rename(temp, activityPath);
  } catch {
    // Monitoring is observational only. Snapshot I/O must never break an MCP call.
  } finally {
    activityWriteInFlight = false;
    if (activityWriteDirty) scheduleActivitySnapshot();
  }
}

function scheduleActivitySnapshot() {
  activityWriteDirty = true;
  if (activityWriteInFlight || activityWriteTimer) return;
  activityWriteTimer = setTimeout(() => {
    activityWriteTimer = null;
    void flushActivitySnapshot();
  }, 50);
}

function writeActivitySnapshot() {
  scheduleActivitySnapshot();
}

function recordRequest(method) {
  const now = Date.now();
  lastRequestAt = now;
  lastRequestMethod = method;
  if (method === 'tools/list') lastListToolsAt = now;
  writeActivitySnapshot();
}

// Clear any stale in-flight snapshot left by a previous crashed launcher.
writeActivitySnapshotSync();

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

// server.js has now loaded the actual terminal-manager module used by the tool
// handlers, while its config paths were still resolved from the isolated home.
// Patch only the command-spawn phase so user shell processes inherit the real
// login-account home without changing Desktop Commander's own config/history.
const { TerminalManager } = await import(pathToFileURL(path.join(dist, 'terminal-manager.js')).href);
const upstreamExecuteCommand = TerminalManager.prototype.executeCommand;
TerminalManager.prototype.executeCommand = function chatxExecuteCommand(...args) {
  const explicitShell = args[2];
  if (!explicitShell) return upstreamExecuteCommand.apply(this, args);
  const isolatedHome = process.env.HOME;
  const isolatedProfile = process.env.USERPROFILE;
  process.env.HOME = realHome;
  process.env.USERPROFILE = realHome;
  try {
    return upstreamExecuteCommand.apply(this, args);
  } finally {
    process.env.HOME = isolatedHome;
    process.env.USERPROFILE = isolatedProfile;
  }
};

await import(pathToFileURL(entry).href);
