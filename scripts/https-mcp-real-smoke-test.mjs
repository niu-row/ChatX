import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import net from 'node:net';

const root = path.resolve('src-tauri/resources');
const node = path.join(root, process.platform === 'win32' ? 'node.exe' : 'node');
const bridge = path.join(root, 'https-mcp-bridge.mjs');
const launcher = path.join(root, 'desktop-commander-launcher.mjs');
const entry = path.join(root, 'desktop-commander/dist/index.js');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-https-real-dc-'));
const settingsPath = path.join(home, 'settings.json');
const authStoreDir = path.join(home, 'oauth-authorizations');
fs.writeFileSync(settingsPath, JSON.stringify({ httpsMcp: { authorizationTtlHours: 168 } }));
const listener = net.createServer();
await new Promise((resolve) => listener.listen(0, '127.0.0.1', resolve));
const port = listener.address().port;
await new Promise((resolve) => listener.close(resolve));
const issuer = 'https://newrow.fun';
const password = crypto.randomBytes(32).toString('hex');
const key = crypto.randomBytes(32).toString('hex');
const base = `http://127.0.0.1:${port}`;
const redirect = 'https://chatgpt.com/connector/oauth/callback';
const verifier = crypto.randomBytes(48).toString('base64url');
const challenge = crypto.createHash('sha256').update(verifier).digest('base64url');
const child = spawn(node, [bridge], {
  env: { ...process.env, CHATX_MCP_ISSUER: issuer, CHATX_MCP_PORT: String(port),
    CHATX_MCP_LOGIN_PASSWORD: password, CHATX_MCP_SIGNING_KEY: key,
    CHATX_MCP_LAUNCHER: launcher, CHATX_MCP_ENTRY: entry,
    CHATX_MCP_NODE: node, CHATX_MCP_HOME: home,
    CHATX_MCP_SETTINGS_PATH: settingsPath, CHATX_MCP_AUTH_STORE: authStoreDir },
  stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true
});
let stdout = '';
let stderr = '';
child.stdout.on('data', c => { stdout += c.toString(); });
child.stderr.on('data', c => { stderr += c.toString(); });
const request = async (url, init = {}) => fetch(base + url, {
  signal: AbortSignal.timeout(6000), redirect: 'manual', ...init
});
try {
  let health = '';
  for (let n = 0; n < 150; n++) {
    if (child.exitCode !== null) throw new Error('Bridge exited: ' + stderr);
    try {
      const r = await request('/healthz');
      if (r.ok) health = (await r.json()).status;
      if (health === 'ready') break;
    } catch { /* cold start */ }
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  if (health !== 'ready') throw new Error('Real Desktop Commander never Ready. stdout: ' + stdout +
    '; stderr: ' + stderr);
  const pub = await request('/.well-known/oauth-authorization-server');
  assert.equal(pub.status, 200);
  const unauth = await request('/mcp', { method: 'POST' });
  assert.equal(unauth.status, 401);
  const registration = await request('/chatx-oauth/register', { method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ redirect_uris: [redirect], token_endpoint_auth_method: 'none' }) });
  assert.equal(registration.status, 201);
  const { client_id } = await registration.json();
  const authorizePath = '/chatx-oauth/authorize?' + new URLSearchParams({
    client_id, redirect_uri: redirect, response_type: 'code', code_challenge: challenge,
    code_challenge_method: 'S256', scope: 'chatx:tools', state: 'smoke',
    resource: issuer + '/mcp'
  });
  const page = await (await request(authorizePath)).text();
  const ticket = /name="ticket" value="([A-Za-z0-9_-]+)"/.exec(page)?.[1];
  assert.ok(ticket, 'Missing authorization ticket');
  const approved = await request('/chatx-oauth/approve', { method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ ticket, password }) });
  assert.equal(approved.status, 302);
  const code = new URL(approved.headers.get('location')).searchParams.get('code');
  const tokenRequest = await request('/chatx-oauth/token', { method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code',
      code, client_id, redirect_uri: redirect, code_verifier: verifier }) });
  assert.equal(tokenRequest.status, 200);
  const token = (await tokenRequest.json()).access_token;
  const rpc = async (id, method, params = {}) => {
    const response = await request('/mcp', { method: 'POST',
      headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json',
        Accept: 'application/json, text/event-stream' },
      body: JSON.stringify({ jsonrpc: '2.0', id, method, params }) });
    assert.equal(response.status, 200);
    const payload = await response.json();
    if (payload.error) throw new Error(JSON.stringify(payload.error));
    return payload.result;
  };
  await rpc(1, 'initialize', { protocolVersion: '2025-06-18', capabilities: {},
    clientInfo: { name: 'runtime-smoke', version: '1' } });
  const tools = (await rpc(2, 'tools/list')).tools;
  assert.equal(tools.length, 26);
  assert.ok(tools.some(t => t.name === 'read_file'));
  console.log('HTTPS MCP real runtime smoke passed: OAuth + 26 bundled Desktop Commander tools');
} finally {
  child.kill();
  await new Promise(resolve => {
    if (child.exitCode !== null) return resolve();
    child.once('exit', resolve);
    setTimeout(resolve, 2000).unref();
  });
  fs.rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
