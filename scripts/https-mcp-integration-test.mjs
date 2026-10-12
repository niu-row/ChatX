// Local behavioral integration test for HTTPS MCP OAuth and stdio forwarding.
// Uses a disposable stdio fixture, never touches the user's files or Tunnel.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { spawn, spawnSync } from 'node:child_process';
import { build } from 'esbuild';
import { createShutdownCoordinator } from './https-mcp-parent-watch.mjs';
import { authorizationStore } from './https-mcp-authorizations.mjs';
import { createMcpRequestBudget } from './mcp-request-budget.mjs';

// A watchdog must still SIGKILL its process group after the leader exits.
// Importing the helper does not launch any child or touch live ChatX.
{
  const signals = [];
  const scheduled = [];
  let exits = [];
  const shutdown = createShutdownCoordinator({
    sendSignal: (signal) => signals.push(signal),
    stopWatching: () => signals.push('stopWatching'),
    schedule: (callback, delay) => scheduled.push({ callback, delay }),
    finish: (code) => exits.push(code)
  });
  shutdown(7); // Process-group leader exited.
  shutdown(0); // GUI also exited during the shutdown grace window.
  assert.deepEqual(signals, ['stopWatching', 'SIGTERM']);
  assert.equal(scheduled.length, 1);
  assert.equal(scheduled[0].delay, 1500);
  assert.deepEqual(exits, [], 'Watchdog must not exit before forced cleanup');
  scheduled[0].callback();
  assert.deepEqual(signals, ['stopWatching', 'SIGTERM', 'SIGKILL']);
  assert.deepEqual(exits, [7]);
}


// Byte budgets are shared across requests; tool slots remain independent
// from control slots. Test exact admission behavior without huge allocations.
{
  const budget = createMcpRequestBudget({ maxRequests: 3, maxControl: 0,
    maxTools: 1, maxPayloadBytes: 6 });
  const first = budget.open(), control = budget.open(), third = budget.open();
  assert.ok(first && control && third);
  assert.equal(budget.open(), null, 'Total request concurrency must be bounded');
  assert.equal(first.reserve(4), true);
  assert.equal(control.reserve(3), false, 'Aggregate body bytes must be bounded');
  assert.equal(control.reserve(2), true);
  assert.equal(first.admitTool(), true);
  assert.equal(third.admitTool(), false, 'Long tool slots must be separately bounded');
  assert.equal(budget.snapshot().requests, 3);
  assert.equal(budget.snapshot().tools, 1);
  assert.equal(budget.snapshot().payloadBytes, 6);
  first.close(); first.close(); // Idempotent release.
  assert.equal(third.admitTool(), true);
  assert.equal(third.reserve(4), true);
  control.close(); third.close();
  assert.equal(budget.snapshot().requests, 0);
  assert.equal(budget.snapshot().payloadBytes, 0);
  // Eight slow ordinary bodies cannot take the four reserved control lanes.
  const guarded = createMcpRequestBudget();
  const slow = Array.from({ length: 8 }, () => guarded.open());
  assert.ok(slow.every(Boolean));
  assert.equal(guarded.open(), null);
  const ping = guarded.open({ controlCandidate: true });
  assert.ok(ping && ping.admit('ping'));
  const smallTool = guarded.open({ controlCandidate: true });
  assert.ok(smallTool);
  assert.equal(smallTool.admit('tools/call'), false,
    'Small tool calls may not occupy reserved control slots');
  assert.equal(smallTool.reserve(4097), false,
    'Reserved control bodies must have a tight size limit');
  smallTool.close(); ping.close();
  slow.forEach((lease) => lease.close());

  const outputs = createMcpRequestBudget({
    maxRequests: 3, maxControl: 0, maxSingleResponseBytes: 100,
    maxBufferedResponseBytes: 120,
  });
  const a = outputs.open(), b = outputs.open();
  assert.equal(a.reserveResponse(80), true);
  assert.equal(b.reserveResponse(50), false, 'Aggregate output buffers must be bounded');
  assert.equal(b.reserveResponse(30), true);
  a.close();
  assert.equal(outputs.snapshot().responseBytes, 110,
    'Response credits remain held until HTTP response completion');
  a.releaseResponse(); b.releaseResponse(); b.close();
  assert.equal(outputs.snapshot().responseBytes, 0);
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-https-mcp-'));
const bundle = path.join(dir, 'bridge.mjs');
const adminBundle = path.join(dir, 'oauth-admin.mjs');
const fixture = path.join(dir, 'fake-mcp.mjs');
const settingsFile = path.join(dir, 'settings.json');
const storeDir = path.join(dir, 'oauth-authorizations');
const cancellationStarted = path.join(dir, 'cancel-started');
const cancellationNotified = path.join(dir, 'cancel-notified');
const cancellationFinished = path.join(dir, 'cancel-finished');
fs.writeFileSync(settingsFile, JSON.stringify({ httpsMcp: { authorizationTtlHours: 168 } }));
const issuer = 'https://newrow.fun';
const redirectUri = 'https://chatgpt.com/connector/oauth/callback';
const password = crypto.randomBytes(32).toString('hex');
const signingKey = crypto.randomBytes(32).toString('hex');
const instanceNonce = crypto.randomBytes(32).toString('hex');
const verifier = crypto.randomBytes(48).toString('base64url');
const challenge = crypto.createHash('sha256').update(verifier).digest('base64url');
const jsonHeaders = { 'content-type': 'application/json' };
let bridge;
try {
  // Expiration of the idempotency window must restore strict replay defense.
  const retryDir = path.join(dir, 'retry-window-fixture');
  const retryStore = authorizationStore({ directory: retryDir, settingsPath: settingsFile });
  const grant = retryStore.createGrant('retry-window-fixture', 168, Date.now(),
    retryStore.revocationGeneration());
  const successor = crypto.randomBytes(32).toString('base64url');
  const issueSecond = Math.floor(Date.now() / 1000);
  assert.equal(retryStore.advanceRefresh(grant.id, null, successor, issueSecond).retried, false);
  assert.deepEqual(retryStore.advanceRefresh(grant.id, null, successor, issueSecond + 1),
    { jti: successor, issuedAt: issueSecond, retried: true });
  const retryRecordPath = path.join(retryDir, 'refresh', grant.id + '.json');
  const retryRecord = JSON.parse(fs.readFileSync(retryRecordPath, 'utf8'));
  retryRecord.retryUntilMs = Date.now() - 1000;
  fs.writeFileSync(retryRecordPath, JSON.stringify(retryRecord));
  assert.throws(() => retryStore.advanceRefresh(grant.id, null, successor, issueSecond + 2),
    /invalid_grant/, 'A stale token used after the retry window must be rejected');
  assert.equal(retryStore.activeGrant(grant.id), null, 'A stale-token replay revokes the grant');

  await build({
    entryPoints: ['scripts/https-mcp-authorizations-admin.mjs'], outfile: adminBundle,
    platform: 'node', format: 'esm', target: 'node20', bundle: true, logLevel: 'silent',
  });
  // Merely opening OAuth Settings must not manufacture a new legacy grant
  // or start the seven-day migration window on a fresh installation.
  const freshAdminDir = path.join(dir, 'new-admin-fixture');
  const fresh = spawnSync(process.execPath, [adminBundle, 'list'], {
    env: { ...process.env, CHATX_MCP_AUTH_STORE: freshAdminDir,
      CHATX_MCP_SETTINGS_PATH: settingsFile },
    encoding: 'utf8',
  });
  assert.equal(fresh.status, 0, fresh.stderr);
  assert.deepEqual(JSON.parse(fresh.stdout).grants, []);
  assert.equal(fs.existsSync(path.join(freshAdminDir, 'legacy.json')), false);

  // The desktop administrative CLI must use the exact same authorization
  // format and revocation rules as the running OAuth bridge.
  const adminDir = path.join(dir, 'admin-fixture');
  const adminStore = authorizationStore({ directory: adminDir, settingsPath: settingsFile });
  const cli = (action, grantId) => {
    const output = spawnSync(process.execPath,
      [adminBundle, action,
        ...(grantId ? [grantId] : [])], {
        env: { ...process.env, CHATX_MCP_AUTH_STORE: adminDir,
          CHATX_MCP_SETTINGS_PATH: settingsFile },
        encoding: 'utf8',
      });
    assert.equal(output.status, 0, output.stderr);
    return JSON.parse(output.stdout);
  };
  // Older persisted grants predate generation markers. Listing must
  // agree with actual token acceptance on both sides of global revocation.
  const oldDir = path.join(dir, 'legacy-revocation-fixture');
  const older = authorizationStore({ directory: oldDir, settingsPath: settingsFile,
    createLegacyIfMissing: false });
  older.revokeAll();
  const revocationAt = JSON.parse(fs.readFileSync(
    path.join(oldDir, 'revoked-all.json'), 'utf8')).revokedAtMs;
  for (const [digit, approvedAtMs, expectedRevoked] of [
    ['a', revocationAt - 3000, true],
    ['b', revocationAt + 1000, false],
  ]) {
    const id = digit.repeat(64);
    fs.writeFileSync(path.join(oldDir, 'grants', id + '.json'), JSON.stringify({
      id, clientHash: 'e'.repeat(64),
      issuedAt: Math.floor(Date.now() / 1000),
      approvedAtMs, expiresAt: Math.floor(Date.now() / 1000) + 3600,
    }));
    assert.equal(older.activeGrant(id) === null, expectedRevoked);
    assert.equal(older.listGrants().grants.find((g) => g.id === id).revoked,
      expectedRevoked, 'Listing must match bearer authorization of legacy grants');
  }

  const managed = adminStore.createGrant('client-a', 168, Date.now(),
    adminStore.revocationGeneration());
  assert.ok(cli('list').grants.some((row) => row.id === managed.id && !row.revoked));
  assert.deepEqual(cli('revoke', managed.id), { revokedId: managed.id });
  assert.equal(cli('list').grants.find((row) => row.id === managed.id).revoked, true);
  const second = adminStore.createGrant('client-b', 168, Date.now(),
    adminStore.revocationGeneration());
  assert.deepEqual(cli('revoke-all'), { revokedAll: true });
  assert.equal(cli('list').grants.find((row) => row.id === second.id).revoked, true);
  assert.equal(adminStore.activeGrant(second.id), null);
  const after = adminStore.createGrant('client-c', 168, Date.now(),
    adminStore.revocationGeneration());
  assert.ok(adminStore.activeGrant(after.id), 'A grant issued after revocation remains valid');
  assert.equal(cli('list').grants.find((row) => row.id === after.id).revoked, false);

  // An unrelated invalid record must NOT convert a durable revocation into
  // a false failure. The read-only list command may still report corruption.
  const damaged = path.join(adminDir, 'grants', 'unrelated.json');
  fs.writeFileSync(damaged, '{broken');
  assert.deepEqual(cli('revoke', after.id), { revokedId: after.id });
  assert.equal(adminStore.activeGrant(after.id), null);
  assert.deepEqual(cli('revoke-all'), { revokedAll: true });
  assert.throws(() => adminStore.listGrants());
  fs.unlinkSync(damaged);

  fs.writeFileSync(fixture, `
import readline from 'node:readline';
import fs from 'node:fs';
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const cancelable = new Map();
for await (const line of rl) {
  const req = JSON.parse(line);
  if (req.method === 'notifications/cancelled') {
    const task = cancelable.get(req.params?.requestId);
    if (task) {
      clearTimeout(task);
      cancelable.delete(req.params.requestId);
      fs.writeFileSync(process.env.CHATX_TEST_CANCEL_NOTIFIED, 'yes');
    }
    continue;
  }
  if (typeof req.id === 'undefined') continue;
  let result;
  if (req.method === 'initialize') {
    result = { protocolVersion: req.params.protocolVersion, capabilities: { tools: {} },
      serverInfo: { name: 'test-fixture', version: '1' } };
  } else if (req.method === 'tools/list') {
    result = { tools: [{ name: 'echo', description: 'Echo fixture',
      inputSchema: { type: 'object', properties: { value: { type: 'string' } } } }] };
  } else if (req.method === 'tools/call') {
    if (req.params.arguments?.value === '__cancel__') {
      fs.writeFileSync(process.env.CHATX_TEST_CANCEL_STARTED, 'yes');
      const timer = setTimeout(() => {
        cancelable.delete(req.id);
        fs.writeFileSync(process.env.CHATX_TEST_CANCEL_FINISHED, 'yes');
        process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: req.id,
          result: { content: [{ type: 'text', text: 'uncanceled' }] } }) + '\\n');
      }, 900);
      cancelable.set(req.id, timer);
      continue;
    }
    if (req.params.arguments?.value === '__hold__') {
      await new Promise((resolve) => setTimeout(resolve, 300));
    }
    result = { content: [{ type: 'text',
      text: req.params.arguments.value === '__large__'
        ? 'x'.repeat(1024 * 1024) : req.params.arguments.value }] };
  } else {
    result = {};
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: req.id, result }) + '\\n');
}`);
  await build({
    entryPoints: ['scripts/https-mcp-bridge.mjs'], outfile: bundle, platform: 'node',
    format: 'esm', target: 'node20', bundle: true, logLevel: 'silent',
    banner: { js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);" }
  });
  const listener = net.createServer();
  await new Promise((resolve) => listener.listen(0, '127.0.0.1', resolve));
  const port = listener.address().port;
  await new Promise((resolve) => listener.close(resolve));
  bridge = spawn(process.execPath, [bundle], {
    cwd: process.cwd(), stdio: ['ignore', 'ignore', 'pipe'],
    env: { ...process.env,
      CHATX_MCP_ISSUER: issuer, CHATX_MCP_PORT: String(port),
      CHATX_MCP_LOGIN_PASSWORD: password, CHATX_MCP_SIGNING_KEY: signingKey,
      CHATX_MCP_APP_VERSION: '9.8.7',
      CHATX_MCP_INSTANCE_NONCE: instanceNonce,
      CHATX_MCP_LAUNCHER: fixture, CHATX_MCP_ENTRY: fixture,
      CHATX_MCP_NODE: process.execPath, CHATX_MCP_HOME: dir,
      CHATX_MCP_SETTINGS_PATH: settingsFile, CHATX_MCP_AUTH_STORE: storeDir,
      CHATX_TEST_CANCEL_STARTED: cancellationStarted,
      CHATX_TEST_CANCEL_NOTIFIED: cancellationNotified,
      CHATX_TEST_CANCEL_FINISHED: cancellationFinished
    }
  });
  let bridgeErrors = '';
  bridge.stderr.on('data', (chunk) => { bridgeErrors += chunk.toString(); });
  const base = `http://127.0.0.1:${port}`;
  const request = async (url, options = {}) => fetch(base + url, {
    signal: AbortSignal.timeout(8000), redirect: 'manual', ...options
  });
  let ready = false;
  for (let i = 0; i < 80; i++) {
    if (bridge.exitCode !== null) throw new Error(`Bridge exited: ${bridgeErrors}`);
    try {
      const res = await request('/healthz');
      if (res.ok) { ready = true; break; }
    } catch { /* starting */ }
    await new Promise((r) => setTimeout(r, 125));
  }
  assert.ok(ready, `Bridge failed to start: ${bridgeErrors}`);
  const untrustedHealth = await (await request('/healthz')).json();
  assert.equal(untrustedHealth.instance, undefined, 'Health identity must not be public');
  const trustedHealth = await (await request('/healthz', {
    headers: { 'x-chatx-instance-nonce': instanceNonce }
  })).json();
  assert.equal(trustedHealth.instance, instanceNonce, 'Bridge must prove its own instance identity');

  const unauthenticated = await request('/mcp', { method: 'POST' });
  assert.equal(unauthenticated.status, 401);
  assert.match(unauthenticated.headers.get('www-authenticate'), /resource_metadata/);

  const discovery = await (await request('/.well-known/oauth-authorization-server')).json();
  assert.equal(discovery.issuer, issuer);
  assert.ok(discovery.code_challenge_methods_supported.includes('S256'));

  const badRegister = await request('/chatx-oauth/register', {
    method: 'POST', headers: jsonHeaders, body: JSON.stringify({ redirect_uris: ['https://evil.example/receive'] })
  });
  assert.equal(badRegister.status, 400, 'OAuth must reject arbitrary redirect hosts');
  const registered = await request('/chatx-oauth/register', {
    method: 'POST', headers: jsonHeaders,
    body: JSON.stringify({ redirect_uris: [redirectUri], token_endpoint_auth_method: 'none' })
  });
  assert.equal(registered.status, 201);
  const { client_id: clientId } = await registered.json();
  const authPath = '/chatx-oauth/authorize?' + new URLSearchParams({
    client_id: clientId, redirect_uri: redirectUri, response_type: 'code',
    code_challenge: challenge, code_challenge_method: 'S256', scope: 'chatx:tools offline_access',
    state: 'known-state', resource: issuer + '/mcp'
  });
  const authResponse = await request(authPath);
  assert.equal(authResponse.status, 200);
  assert.doesNotMatch(authResponse.headers.get('content-security-policy'), /form-action/i,
    'Authorization POST redirects to ChatGPT, which can itself redirect; CSP must not restrict form-action');
  assert.match(authResponse.headers.get('content-security-policy'), /default-src 'none'/);
  assert.match(authResponse.headers.get('content-security-policy'), /frame-ancestors 'none'/);
  const authPage = await authResponse.text();
  const ticket = /name="ticket" value="([A-Za-z0-9_-]+)"/.exec(authPage)?.[1];
  assert.ok(ticket, 'OAuth authorization form must carry a pending ticket');
  const badApproval = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket, password: 'incorrect-password' })
  });
  assert.equal(badApproval.status, 403);
  const correctPage = await (await request(authPath)).text();
  const newTicket = /name="ticket" value="([A-Za-z0-9_-]+)"/.exec(correctPage)?.[1];
  const approved = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket: newTicket, password })
  });
  assert.equal(approved.status, 302);
  const callback = new URL(approved.headers.get('location'));
  assert.equal(callback.host, 'chatgpt.com');
  assert.equal(callback.searchParams.get('state'), 'known-state');
  const code = callback.searchParams.get('code');
  assert.ok(code);

  const invalidToken = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code', code,
      client_id: clientId, redirect_uri: redirectUri, code_verifier: 'bad-verifier' })
  });
  assert.equal(invalidToken.status, 400, 'PKCE verifier must be checked');
  // Bad verification consumes a one-use code; start fresh.
  const freshPage = await (await request(authPath)).text();
  const freshTicket = /name="ticket" value="([A-Za-z0-9_-]+)"/.exec(freshPage)?.[1];
  const freshApproval = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket: freshTicket, password })
  });
  const freshCode = new URL(freshApproval.headers.get('location')).searchParams.get('code');
  const tokensResponse = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code', code: freshCode,
      client_id: clientId, redirect_uri: redirectUri, code_verifier: verifier })
  });
  assert.equal(tokensResponse.status, 200);
  const tokens = await tokensResponse.json();
  assert.ok(tokens.access_token && tokens.refresh_token);
  assert.equal(tokens.scope, 'chatx:tools offline_access');
  const tokenPayload = (jwt) => JSON.parse(Buffer.from(jwt.split('.')[0], 'base64url').toString('utf8'));
  const firstAccess = tokenPayload(tokens.access_token);
  const firstRefresh = tokenPayload(tokens.refresh_token);
  assert.match(firstAccess.gid, /^[0-9a-f]{64}$/);
  const firstRecord = JSON.parse(fs.readFileSync(path.join(storeDir, 'grants', `${firstAccess.gid}.json`)));
  assert.ok(firstRecord.expiresAt - firstRecord.issuedAt === 7 * 24 * 3600,
    'Seven-day authorization must have an absolute deadline');
  assert.equal(firstRefresh.gid, firstAccess.gid);
  assert.equal(firstRefresh.exp, firstRecord.expiresAt, 'Refresh may not outlive a finite authorization');
  const authHeaders = { ...jsonHeaders, authorization: `Bearer ${tokens.access_token}` };
  const rpc = async (id, method, params = {}) => {
    const res = await request('/mcp', { method: 'POST', headers: authHeaders,
      body: JSON.stringify({ jsonrpc: '2.0', id, method, params }) });
    assert.equal(res.status, 200, method);
    return res.json();
  };
  const init = await rpc(1, 'initialize', {
    protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'test', version: '1' }
  });
  assert.equal(init.result.protocolVersion, '2025-06-18');
  assert.equal(init.result.serverInfo.version, '9.8.7',
    'MCP protocol version metadata must come from the desktop runtime');
  const tools = await rpc(2, 'tools/list');
  assert.equal(tools.result.tools[0].name, 'echo');
  assert.equal(tools.result.tools[0].securitySchemes?.[0].type, 'oauth2');
  assert.deepEqual(tools.result.tools[0].securitySchemes?.[0].scopes, ['chatx:tools']);
  const called = await rpc(3, 'tools/call', { name: 'echo', arguments: { value: 'https-mcp-ok' } });
  const cancel = new AbortController();
  const cancelledRequest = request('/mcp', { method: 'POST',
    signal: cancel.signal, headers: authHeaders,
    body: JSON.stringify({ jsonrpc: '2.0', id: 333, method: 'tools/call',
      params: { name: 'echo', arguments: { value: '__cancel__' } } }),
  }).catch(() => null);
  for (let attempt = 0; attempt < 40 && !fs.existsSync(cancellationStarted); attempt++) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.ok(fs.existsSync(cancellationStarted), 'Test MCP tool must start');
  cancel.abort();
  await cancelledRequest;
  for (let attempt = 0; attempt < 45 && !fs.existsSync(cancellationNotified); attempt++) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.ok(fs.existsSync(cancellationNotified),
    'Aborted WSS/HTTP requests must send MCP notifications/cancelled to stdio');
  await new Promise((resolve) => setTimeout(resolve, 930));
  assert.equal(fs.existsSync(cancellationFinished), false,
    'A cooperative Desktop Commander fixture must stop when notified');

  // Explicit JSON-RPC cancellation can arrive on a separate stateless HTTP
  // request. Bind it to the same authorization, not merely to its request ID.
  for (const file of [cancellationStarted, cancellationNotified, cancellationFinished]) {
    fs.rmSync(file, { force: true });
  }
  const explicitlyCancelled = request('/mcp', { method: 'POST', headers: authHeaders,
    body: JSON.stringify({ jsonrpc: '2.0', id: 334, method: 'tools/call',
      params: { name: 'echo', arguments: { value: '__cancel__' } } }),
  });
  for (let i = 0; i < 60 && !fs.existsSync(cancellationStarted); i++) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.ok(fs.existsSync(cancellationStarted));

  const storeForIsolation = authorizationStore({ directory: storeDir,
    settingsPath: settingsFile, createLegacyIfMissing: false });
  const otherGrant = storeForIsolation.createGrant('unrelated-client', 168,
    Date.now(), storeForIsolation.revocationGeneration());
  const otherPayload = Buffer.from(JSON.stringify({
    ...firstAccess, gid: otherGrant.id, sub: 'unrelated-client',
  })).toString('base64url');
  const otherAccess = otherPayload + '.' + crypto.createHmac('sha256', signingKey)
    .update(otherPayload).digest('base64url');
  const cancelNotice = { jsonrpc: '2.0', method: 'notifications/cancelled',
    params: { requestId: 334, reason: 'user stopped' } };
  const unrelatedCancel = await request('/mcp', { method: 'POST',
    headers: { ...jsonHeaders, authorization: 'Bearer ' + otherAccess },
    body: JSON.stringify(cancelNotice),
  });
  assert.equal(unrelatedCancel.status, 202);
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.equal(fs.existsSync(cancellationNotified), false,
    'A different OAuth grant must never cancel another grant tool');
  const ownCancel = await request('/mcp', { method: 'POST', headers: authHeaders,
    body: JSON.stringify(cancelNotice),
  });
  assert.equal(ownCancel.status, 202);
  const canceledResult = await explicitlyCancelled;
  assert.equal(canceledResult.status, 200);
  assert.equal((await canceledResult.json()).error.code, -32800);
  for (let i = 0; i < 60 && !fs.existsSync(cancellationNotified); i++) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.ok(fs.existsSync(cancellationNotified),
    'Explicit notification must propagate to cooperative stdio tools');
  await new Promise((resolve) => setTimeout(resolve, 930));
  assert.equal(fs.existsSync(cancellationFinished), false);

  // Streaming transport supports 64 requests, but Node's JSON-RPC materializes
  // results. The local Bridge must refuse excess work instead of buffering
  // 64 * 64 MiB in the same process.
  const competingTools = Array.from({ length: 11 }, (_, i) =>
    request('/mcp', {
      method: 'POST', headers: authHeaders,
      body: JSON.stringify({ jsonrpc: '2.0', id: 300 + i,
        method: 'tools/call', params: { name: 'echo', arguments: { value: '__hold__' } } }),
    })
  );
  await new Promise((resolve) => setTimeout(resolve, 50));
  const pingWhileToolsBusy = await rpc(500, 'ping');
  assert.deepEqual(pingWhileToolsBusy.result, {},
    'Control requests must remain serviceable under tool saturation');
  const concurrent = await Promise.all(competingTools);
  const statuses = concurrent.map((response) => response.status);
  assert.ok(statuses.includes(429), 'Excess MCP JSON requests must get bounded 429');
  assert.ok(statuses.includes(200), 'Accepted MCP requests must continue normally');

  // Deliberately leave eight large-body HTTP uploads incomplete. Previously
  // they consumed all twelve admission slots, preventing a new ping.
  const heldSockets = await Promise.all(Array.from({ length: 8 }, () =>
    new Promise((resolve, reject) => {
      const socket = net.createConnection({ host: '127.0.0.1', port });
      socket.once('error', reject);
      socket.once('connect', () => {
        socket.off('error', reject);
        socket.on('error', () => {});
        socket.write('POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:' + port +
          '\r\nAuthorization: Bearer ' + tokens.access_token +
          '\r\nContent-Type: application/json\r\nContent-Length: 8192' +
          '\r\nConnection: keep-alive\r\n\r\n{');
        resolve(socket);
      });
    })
  ));
  try {
    await new Promise((resolve) => setTimeout(resolve, 120));
    assert.deepEqual((await rpc(501, 'ping')).result, {},
      'Slow ordinary uploads must not consume reserved control request slots');
  } finally {
    heldSockets.forEach((socket) => socket.destroy());
  }
  await new Promise((resolve) => setTimeout(resolve, 60));

  // Large SDK tool results must still round-trip without a second
  // JSON.stringify copy, and simultaneous result buffering is budgeted.
  const largeResults = await Promise.all(Array.from({ length: 6 }, (_, i) =>
    rpc(600 + i, 'tools/call', { name: 'echo', arguments: { value: '__large__' } })
  ));
  assert.ok(largeResults.every((result) =>
    result.result.content[0].text.length === 1024 * 1024));

  assert.equal(called.result.content[0].text, 'https-mcp-ok');
  const forbiddenOrigin = await request('/mcp', { method: 'POST',
    headers: { ...authHeaders, origin: 'https://untrusted.example' }, body: '{}' });
  assert.equal(forbiddenOrigin.status, 403);
  const sse = await request('/mcp', { headers: authHeaders });
  assert.equal(sse.status, 405);
  const refreshed = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token', client_id: clientId,
      refresh_token: tokens.refresh_token })
  });
  assert.equal(refreshed.status, 200);
  const rotatedFinite = await refreshed.json();
  assert.notEqual(rotatedFinite.refresh_token, tokens.refresh_token,
    'Refresh must rotate the signed token even for time-limited grants');
  assert.notEqual(tokenPayload(rotatedFinite.refresh_token).jti,
    tokenPayload(tokens.refresh_token).jti);
  // Changing the default does not retroactively extend an existing grant.
  fs.writeFileSync(settingsFile, JSON.stringify({ httpsMcp: { authorizationTtlHours: null } }));
  const stillValid = await rpc(4, 'ping');
  assert.deepEqual(stillValid.result, {});
  const infiniteAuth = await request(authPath);
  const infinitePage = await infiniteAuth.text();
  assert.match(infinitePage, /不限时间/, 'Consent page must disclose unlimited access');
  const infiniteTicket = /name="ticket" value="([A-Za-z0-9_-]+)"/.exec(infinitePage)?.[1];
  const infiniteApproval = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket: infiniteTicket, password })
  });
  assert.equal(infiniteApproval.status, 302);
  const infiniteCode = new URL(infiniteApproval.headers.get('location')).searchParams.get('code');
  const infiniteResponse = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code',
      code: infiniteCode, client_id: clientId, redirect_uri: redirectUri, code_verifier: verifier })
  });
  assert.equal(infiniteResponse.status, 200);
  const infiniteTokens = await infiniteResponse.json();
  const infiniteAccess = tokenPayload(infiniteTokens.access_token);
  const infiniteRefresh = tokenPayload(infiniteTokens.refresh_token);
  const infiniteRecord = JSON.parse(fs.readFileSync(
    path.join(storeDir, 'grants', infiniteAccess.gid + '.json'), 'utf8'
  ));
  assert.equal(infiniteRecord.expiresAt, null);
  assert.equal(infiniteRefresh.exp, null, 'Unlimited consent must not expire due to inactivity');
  assert.ok(infiniteAccess.exp > Math.floor(Date.now() / 1000),
    'Access token must remain short-lived even for unlimited consent');
  const infiniteHeader = { ...jsonHeaders, authorization: 'Bearer ' + infiniteTokens.access_token };
  const infiniteCall = () => request('/mcp', { method: 'POST', headers: infiniteHeader,
    body: JSON.stringify({ jsonrpc: '2.0', id: 10, method: 'ping', params: {} }) });
  assert.equal((await infiniteCall()).status, 200);
  const infiniteRenew = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token', client_id: clientId,
      refresh_token: infiniteTokens.refresh_token })
  });
  assert.equal(infiniteRenew.status, 200, 'Unlimited refresh token should renew');
  const rotatedInfinite = await infiniteRenew.json();
  assert.notEqual(rotatedInfinite.refresh_token, infiniteTokens.refresh_token);
  const reused = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token', client_id: clientId,
      refresh_token: infiniteTokens.refresh_token })
  });
  assert.equal(reused.status, 200, 'A short retry must not revoke a valid OAuth grant');
  const retriedInfinite = await reused.json();
  assert.equal(retriedInfinite.refresh_token, rotatedInfinite.refresh_token,
    'A retry after a lost HTTP response must return the same successor token');
  assert.equal(retriedInfinite.access_token, rotatedInfinite.access_token,
    'The access token must also be stable across a retry');
  assert.equal(fs.existsSync(path.join(storeDir, 'revoked', infiniteAccess.gid)), false);
  const renewAgain = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token', client_id: clientId,
      refresh_token: rotatedInfinite.refresh_token })
  });
  assert.equal(renewAgain.status, 200, 'The rotated token should remain usable');
  const replayAfterSuccessor = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token', client_id: clientId,
      refresh_token: infiniteTokens.refresh_token })
  });
  assert.equal(replayAfterSuccessor.status, 400,
    'Replaying a token after its successor has advanced is a compromise signal');
  assert.ok(fs.existsSync(path.join(storeDir, 'revoked', infiniteAccess.gid)),
    'Real replay should revoke the entire refresh family');
  assert.equal((await infiniteCall()).status, 401,
    'Revoked unlimited access token must be rejected immediately');
  const revokedRenew = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token', client_id: clientId,
      refresh_token: infiniteTokens.refresh_token })
  });
  assert.equal(revokedRenew.status, 400, 'Revoked unlimited refresh must be rejected');

  // A revoked authorization invalidates even an otherwise unexpired one-hour access token.
  fs.writeFileSync(path.join(storeDir, 'grants', firstAccess.gid + '.json'),
    JSON.stringify({ ...firstRecord, expiresAt: Math.floor(Date.now() / 1000) - 1 }));
  const expiredAccess = await request('/mcp', { method: 'POST', headers: authHeaders,
    body: JSON.stringify({ jsonrpc: '2.0', id: 11, method: 'ping' }) });
  assert.equal(expiredAccess.status, 401, 'Absolute consent expiration must override the token lifetime');
  // A pre-upgrade token without a grant ID is only accepted during the migration window.
  const signLegacy = (data) => {
    const body = Buffer.from(JSON.stringify(data)).toString('base64url');
    const sig = crypto.createHmac('sha256', signingKey).update(body).digest('base64url');
    return body + '.' + sig;
  };
  const oldAccess = signLegacy({ kind: 'access', iss: issuer,
    aud: issuer + '/mcp', scope: 'chatx:tools', sub: 'old-client',
    exp: Math.floor(Date.now() / 1000) + 600 });
  const oldHeaders = { ...jsonHeaders, authorization: 'Bearer ' + oldAccess };
  const oldCall = () => request('/mcp', { method: 'POST', headers: oldHeaders,
    body: JSON.stringify({ jsonrpc: '2.0', id: 12, method: 'ping' }) });
  assert.equal((await oldCall()).status, 200, 'Valid legacy tokens need an upgrade grace period');
  const legacyRefresh = signLegacy({ kind: 'refresh', iss: issuer,
    aud: issuer + '/mcp', scope: 'chatx:tools', sub: 'old-client', clientId,
    exp: Math.floor(Date.now() / 1000) + 600 });
  const migrateLegacy = () => request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token',
      client_id: clientId, refresh_token: legacyRefresh })
  });
  const migrated = await migrateLegacy();
  assert.equal(migrated.status, 200, 'Pre-upgrade refresh should migrate once');
  const migratedTokens = await migrated.json();
  assert.ok(tokenPayload(migratedTokens.refresh_token).jti,
    'Migrated refresh must gain a one-time token identity');
  const migratedRetry = await migrateLegacy();
  assert.equal(migratedRetry.status, 200,
    'A lost response during legacy refresh migration must be retryable');
  assert.equal((await migratedRetry.json()).refresh_token, migratedTokens.refresh_token);
  const secondLegacy = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token',
      client_id: clientId, refresh_token: migratedTokens.refresh_token })
  });
  assert.equal(secondLegacy.status, 200);
  assert.equal((await migrateLegacy()).status, 400,
    'Replaying legacy refresh after its successor advanced must revoke the grant');
  fs.writeFileSync(path.join(storeDir, 'revoked', 'legacy'), 'revoked');
  assert.equal((await oldCall()).status, 401, 'Legacy authorization must be revocable');

  // "Revoke all" must also invalidate codes approved but not yet exchanged.
  const preRevokePage = await (await request(authPath)).text();
  const preRevokeTicket = /name="ticket" value="([A-Za-z0-9_-]+)"/.exec(preRevokePage)?.[1];
  assert.ok(preRevokeTicket);
  const staleTicketPage = await request(authPath);
  assert.equal(staleTicketPage.status, 200);
  const staleTicket = /name="ticket" value="([A-Za-z0-9_-]+)"/
    .exec(await staleTicketPage.text())?.[1];
  assert.ok(staleTicket);
  const preRevokeApproval = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket: preRevokeTicket, password })
  });
  const preRevokeCode = new URL(preRevokeApproval.headers.get('location')).searchParams.get('code');
  fs.writeFileSync(path.join(storeDir, 'revoked-all.json'),
    JSON.stringify({ revokedAtMs: Date.now(), generation: crypto.randomBytes(32).toString('hex') }));
  const staleApproval = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket: staleTicket, password })
  });
  assert.equal(staleApproval.status, 400, 'Global revoke must cancel pending consent tickets');
  const preRevokeExchange = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code', code: preRevokeCode,
      client_id: clientId, redirect_uri: redirectUri, code_verifier: verifier })
  });
  assert.equal(preRevokeExchange.status, 400,
    'Global revocation generation must cancel outstanding authorization codes');
  // Fresh consent after global revocation remains possible. The next global
  // revocation invalidates both of its tokens without trusting clock order.
  const freshAfterPage = await request(authPath);
  assert.equal(freshAfterPage.status, 200);
  const afterTicket = /name="ticket" value="([A-Za-z0-9_-]+)"/
    .exec(await freshAfterPage.text())?.[1];
  assert.ok(afterTicket);
  const afterApproval = await request('/chatx-oauth/approve', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket: afterTicket, password })
  });
  const afterCode = new URL(afterApproval.headers.get('location')).searchParams.get('code');
  const afterResponse = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code', code: afterCode,
      client_id: clientId, redirect_uri: redirectUri, code_verifier: verifier })
  });
  assert.equal(afterResponse.status, 200, 'Consent after global revocation must work');
  const afterTokens = await afterResponse.json();
  const callAfter = () => request('/mcp', { method: 'POST',
    headers: { ...jsonHeaders, authorization: 'Bearer ' + afterTokens.access_token },
    body: JSON.stringify({ jsonrpc: '2.0', id: 13, method: 'ping' }) });
  assert.equal((await callAfter()).status, 200);
  fs.writeFileSync(path.join(storeDir, 'revoked-all.json'),
    JSON.stringify({ revokedAtMs: Date.now(), generation: crypto.randomBytes(32).toString('hex') }));
  assert.equal((await callAfter()).status, 401, 'Second global revocation must reject access tokens');
  const afterRenew = await request('/chatx-oauth/token', {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'refresh_token',
      client_id: clientId, refresh_token: afterTokens.refresh_token })
  });
  assert.equal(afterRenew.status, 400, 'Second global revocation must reject refresh tokens');
  // Invalid requests spread across a modest number of IPs must not
  // exhaust the global budget and block a legitimate OAuth login.
  for (let index = 1; index <= 30; index++) {
    const response = await request('/chatx-oauth/authorize?response_type=invalid', {
      headers: { 'x-forwarded-for': `198.51.100.${index}` }
    });
    assert.equal(response.status, 400,
      'Distributed invalid requests should remain subject to their own IP buckets');
  }
  const authorizedAfterNoise = await request(authPath);
  assert.equal(authorizedAfterNoise.status, 200,
    'An ordinary consent must not be blocked by 30 distributed invalid requests');
  // Public OAuth endpoints must rate-limit unauthenticated requests without
  // requiring an attacker to know a client ID, password or redirect URI.
  let rateLimitedAuthorize = false;
  for (let i = 0; i < 32; i++) {
    const response = await request('/chatx-oauth/authorize?response_type=invalid');
    if (response.status === 429) { rateLimitedAuthorize = true; break; }
    assert.equal(response.status, 400);
  }
  assert.equal(rateLimitedAuthorize, true, 'OAuth authorize must be rate-limited');
  let rateLimitedRegister = false;
  for (let i = 0; i < 15; i++) {
    const response = await request('/chatx-oauth/register', {
      method: 'POST', headers: jsonHeaders, body: JSON.stringify({ redirect_uris: [] })
    });
    if (response.status === 429) { rateLimitedRegister = true; break; }
    assert.equal(response.status, 400);
  }
  assert.equal(rateLimitedRegister, true, 'Dynamic OAuth registration must be rate-limited');
  console.log('HTTPS MCP integration passed: OAuth discovery, DCR, PKCE, bearer gate, tools/list, tools/call, refresh, origin guard');
} finally {
  if (bridge) {
    bridge.kill();
    await new Promise((resolve) => { if (bridge.exitCode !== null) return resolve();
      bridge.once('exit', resolve); setTimeout(resolve, 2000); });
  }
  fs.rmSync(dir, { recursive: true, force: true });
}
