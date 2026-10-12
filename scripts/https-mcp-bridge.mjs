// ChatX HTTPS MCP bridge. Bind only to loopback; publish through authenticated HTTPS reverse proxy.
import http from 'node:http';
import crypto from 'node:crypto';
import { authorizationStore } from './https-mcp-authorizations.mjs';
import { createMcpRequestBudget } from './mcp-request-budget.mjs';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const issuer = (process.env.CHATX_MCP_ISSUER || '').replace(/\/$/, '');
const port = Number(process.env.CHATX_MCP_PORT || 3000);
const password = process.env.CHATX_MCP_LOGIN_PASSWORD || '';
const signingKey = process.env.CHATX_MCP_SIGNING_KEY || '';
const instanceNonce = process.env.CHATX_MCP_INSTANCE_NONCE || '';
const appVersion = /^\d+\.\d+\.\d+$/.test(process.env.CHATX_MCP_APP_VERSION || '')
  ? process.env.CHATX_MCP_APP_VERSION : 'unknown';
const launcher = process.env.CHATX_MCP_LAUNCHER;
const home = process.env.CHATX_MCP_HOME;
const entry = process.env.CHATX_MCP_ENTRY;
const node = process.env.CHATX_MCP_NODE || process.execPath;
const resource = `${issuer}/mcp`;
const scope = 'chatx:tools';
const pending = new Map();
const codes = new Map();
const attempts = new Map();
const rateBuckets = new Map();
// Eight long-running tools plus four reserved control requests. Accounting
// also bounds buffered incoming body bytes across all active JSON requests.
// This is not a cap on Desktop Commander output or V8 heap usage.
const requestBudget = createMcpRequestBudget();
// Stateless MCP JSON-RPC IDs are unique only within an authorization. Never
// allow a different grant to cancel another grant's in-flight request.
const activeOperations = new Map();
function operationKey(claim, id) {
  if (typeof id === 'string' && id.length > 0 && id.length <= 128) {
    return JSON.stringify([claim.gid ?? 'legacy', claim.sub ?? '', 'string', id]);
  }
  if (typeof id === 'number' && Number.isSafeInteger(id)) {
    return JSON.stringify([claim.gid ?? 'legacy', claim.sub ?? '', 'number', id]);
  }
  return null;
}
let authStore;
let client;
let transport;
let connecting;
if (!/^https:\/\/[a-z0-9.-]+(?::443)?$/i.test(issuer) || !password || password.length < 24 ||
    signingKey.length < 40 || !launcher || !home || !entry || !Number.isInteger(port) ||
    port < 1024 || port > 65535) {
  throw new Error('ChatX HTTPS MCP configuration incomplete or unsafe');
}
authStore = authorizationStore({
  directory: process.env.CHATX_MCP_AUTH_STORE,
  settingsPath: process.env.CHATX_MCP_SETTINGS_PATH
});
const b64 = (v) => Buffer.from(v).toString('base64url');
const hmac = (v) => crypto.createHmac('sha256', signingKey).update(v).digest('base64url');
const constantEq = (a, b) => {
  const left = Buffer.from(String(a));
  const right = Buffer.from(String(b));
  return left.length === right.length && crypto.timingSafeEqual(left, right);
};
const sign = (data) => {
  const value = b64(JSON.stringify(data));
  return `${value}.${hmac(value)}`;
};
const verify = (token) => {
  if (!/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token || '')) return null;
  const [payload, mac] = token.split('.');
  if (!constantEq(hmac(payload), mac)) return null;
  try {
    const data = JSON.parse(Buffer.from(payload, 'base64url').toString('utf8'));
    if (data?.kind === 'client') return data; // OAuth client_id is public, not a bearer credential.
    if (data?.kind === 'refresh' && data.exp === null && /^[0-9a-f]{64}$/.test(data.gid || '')) {
      return data; // Unlimited consent, revocable by the persisted grant record.
    }
    return Number.isSafeInteger(data?.exp) && data.exp > Math.floor(Date.now() / 1000) ? data : null;
  } catch { return null; }
};
const random = () => crypto.randomBytes(32).toString('base64url');
const json = (res, status, data, headers = {}) => {
  res.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store',
    'x-content-type-options': 'nosniff', ...headers });
  res.end(JSON.stringify(data));
};
const text = (res, status, data, headers = {}) => {
  res.writeHead(status, { 'content-type': 'text/plain; charset=utf-8',
    'cache-control': 'no-store', 'x-content-type-options': 'nosniff', ...headers });
  res.end(data);
};
const oauthError = (res, status, error) => json(res, status, { error });
const validRedirect = (value) => {
  try { const url = new URL(value); return url.protocol === 'https:' &&
    (url.hostname === 'chatgpt.com' || url.hostname === 'chat.openai.com') &&
    !url.username && !url.password && !url.hash; } catch { return false; }
};
const clientRedirects = (id) => {
  if (id === 'https://chatgpt.com/oauth/client.json' ||
      /^https:\/\/chatgpt\.com\/oauth\/[a-zA-Z0-9_-]+\/client\.json$/.test(id)) return null;
  const data = verify(id.startsWith('chatx.') ? id.slice(6) : '');
  return data?.kind === 'client' && Array.isArray(data.redirects) ? data.redirects : false;
};
// JSON-RPC parsing still requires a bounded body, while WSS v2 streams to
// this loopback endpoint without a base64 copy or gateway-side accumulation.
async function readBody(req, limit = 64 * 1024 * 1024, onChunk) {
  let length = 0;
  const chunks = [];
  for await (const chunk of req) {
    length += chunk.length;
    if (length > limit) {
      const error = new Error('Request body too large');
      error.statusCode = 413;
      throw error;
    }
    onChunk?.(chunk.length);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}
async function form(req) {
  const body = await readBody(req, 32 * 1024);
  if ((req.headers['content-type'] || '').includes('application/json')) return JSON.parse(body);
  return Object.fromEntries(new URLSearchParams(body));
}
function html(res, body, status = 200) {
  res.writeHead(status, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store',
    // Do not set form-action: OAuth callbacks can redirect via chatgpt.com to platform.openai.com.
    // CSP applies form-action across the redirect chain, not only to the initial POST.
    'content-security-policy': "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
    'x-frame-options': 'DENY', 'x-content-type-options': 'nosniff', 'referrer-policy': 'no-referrer' });
  res.end(body);
}
function challenge(res) {
  return json(res, 401, { error: 'unauthorized' }, {
    'www-authenticate': `Bearer resource_metadata="${issuer}/.well-known/oauth-protected-resource/mcp", scope="${scope}"`
  });
}
function cleanup() {
  const now = Date.now();
  for (const [key, v] of pending) if (v.until < now) pending.delete(key);
  for (const [key, v] of codes) if (v.until < now) codes.delete(key);
  for (const [key, v] of attempts) if (v.until < now) attempts.delete(key);
  for (const [key, v] of rateBuckets) if (v.until < now) rateBuckets.delete(key);
}
function clientAddress(req) {
  // The deployment's Nginx overwrites X-Forwarded-For with $remote_addr.
  return String(req.headers['x-forwarded-for'] || req.socket.remoteAddress || 'local')
    .split(',')[0].trim().slice(0, 100);
}
function rateLimited(req, category, perIp, global, res) {
  const now = Date.now();
  const keys = [`${category}:all`, `${category}:${clientAddress(req)}`];
  const limits = [global, perIp];
  if (rateBuckets.size >= 2048 && keys.some((key) => !rateBuckets.has(key))) {
    // The global volume guard below bounds growth well before this point in
    // ordinary traffic. Expired per-IP entries should not lock out new users.
    cleanup();
    if (rateBuckets.size >= 2048) {
      text(res, 429, 'Too many requests', { 'retry-after': '60' });
      return true;
    }
  }
  for (let i = 0; i < keys.length; i++) {
    const bucket = rateBuckets.get(keys[i]);
    if (bucket && bucket.until > now && bucket.count >= limits[i]) {
      text(res, 429, 'Too many requests', { 'retry-after': String(Math.max(1, Math.ceil((bucket.until - now) / 1000))) });
      return true;
    }
  }
  for (const key of keys) {
    const bucket = rateBuckets.get(key);
    if (bucket && bucket.until > now) bucket.count++;
    else rateBuckets.set(key, { count: 1, until: now + 60_000 });
  }
  return false;
}
setInterval(cleanup, 60_000).unref();
async function ensureClient() {
  if (client) return client;
  if (connecting) return connecting;
  connecting = (async () => {
    const cleanEnv = { ...process.env };
    for (const key of Object.keys(cleanEnv)) {
      if (key.startsWith('CHATX_MCP_') || key === 'CHATX_TUNNEL_RUNTIME_KEY') delete cleanEnv[key];
    }
    const nextTransport = new StdioClientTransport({
      command: node, args: [launcher, home, entry, '--no-onboarding'], env: cleanEnv, stderr: 'inherit'
    });
    const nextClient = new Client({ name: 'chatx-https-bridge', version: '1.0.0' });
    try {
      await nextClient.connect(nextTransport);
      nextTransport.onclose = () => { if (transport === nextTransport) { client = null; transport = null; } };
      client = nextClient;
      transport = nextTransport;
      return nextClient;
    } catch (error) {
      await nextTransport.close().catch(() => {});
      throw error;
    }
  })().finally(() => { connecting = null; });
  return connecting;
}
const authMetadata = {
  issuer, authorization_endpoint: `${issuer}/chatx-oauth/authorize`,
  token_endpoint: `${issuer}/chatx-oauth/token`,
  registration_endpoint: `${issuer}/chatx-oauth/register`,
  response_types_supported: ['code'], response_modes_supported: ['query'],
  grant_types_supported: ['authorization_code', 'refresh_token'],
  token_endpoint_auth_methods_supported: ['none'], code_challenge_methods_supported: ['S256'],
  scopes_supported: [scope, 'offline_access']
};
const resourceMetadata = { resource, authorization_servers: [issuer], scopes_supported: [scope],
  bearer_methods_supported: ['header'] };
function authorizePage(req, res, url) {
  if (rateLimited(req, 'authorize', 12, 1200, res)) return;
  const params = url.searchParams;
  const clientId = params.get('client_id') || '';
  const redirectUri = params.get('redirect_uri') || '';
  const allowedRedirects = clientRedirects(clientId);
  if (allowedRedirects === false || !validRedirect(redirectUri) ||
      (allowedRedirects && !allowedRedirects.includes(redirectUri)) ||
      params.get('response_type') !== 'code' || params.get('code_challenge_method') !== 'S256' ||
      !/^[A-Za-z0-9_-]{43,128}$/.test(params.get('code_challenge') || '') ||
      !params.get('state') || params.get('state').length > 1024 ||
      ![scope, `${scope} offline_access`, `offline_access ${scope}`, ''].includes(params.get('scope') || '') ||
      (params.has('resource') && params.get('resource') !== resource)) {
    return html(res, '<h1>Invalid OAuth authorization request</h1>', 400);
  }
  if (pending.size >= 300) return text(res, 429, 'Too many pending authorizations');
  const ttlHours = authStore.lifetimeHours();
  const lifetimeLabel = ttlHours === null ? '不限时间（直到在本机撤销授权）'
    : `${ttlHours} 小时（总授权期固定，不会因刷新令牌而延长）`;
  const ticket = random();
  pending.set(ticket, { clientId, redirectUri, state: params.get('state'), ttlHours,
    issuedGeneration: authStore.revocationGeneration(),
    grantedScope: params.get('scope') || scope,
    challenge: params.get('code_challenge'), until: Date.now() + 10 * 60_000 });
  html(res, `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>ChatX OAuth 授权</title><style>body{font:16px system-ui;max-width:500px;margin:10vh auto;padding:24px;line-height:1.7}
input,button{font:inherit;padding:12px;width:100%;box-sizing:border-box;margin-top:8px}
button{background:#1857ac;color:white;border:0;cursor:pointer}p{color:#555}</style>
<h1>授权 ChatGPT 访问 ChatX</h1><p>授权后 ChatGPT 能通过 Desktop Commander 读取、修改本机文件及执行命令。
仅在你主动创建此连接时授权。</p><p><strong>本次授权期限：${lifetimeLabel}</strong></p>
<form action="/chatx-oauth/approve" method="post"><input type="hidden" name="ticket" value="${ticket}">
<label>ChatX HTTPS 授权密码<input name="password" type="password" required autocomplete="off"></label>
<button type="submit">确认授权</button></form></html>`);
}
async function approve(req, res) {
  if (rateLimited(req, 'approve', 20, 1200, res)) return;
  const p = await form(req);
  const ticket = String(p.ticket || '');
  const data = pending.get(ticket);
  pending.delete(ticket);
  if (!data || data.until < Date.now() ||
      data.issuedGeneration !== authStore.revocationGeneration()) {
    return html(res, '<h1>授权请求已过期或已被撤销</h1>', 400);
  }
  const addr = (req.headers['x-forwarded-for'] || req.socket.remoteAddress || '').toString().split(',')[0].trim();
  const state = attempts.get(addr) || { count: 0, until: Date.now() + 300_000 };
  if (state.until < Date.now()) { state.count = 0; state.until = Date.now() + 300_000; }
  if (state.count >= 8 || !constantEq(p.password || '', password)) {
    state.count++;
    attempts.set(addr, state);
    return html(res, '<h1>密码错误或尝试次数过多，请重新发起授权</h1>', 403);
  }
  attempts.delete(addr);
  const code = random();
  codes.set(code, { ...data, approvedAtMs: Date.now(),
    approvedGeneration: authStore.revocationGeneration(), until: Date.now() + 90_000 });
  const redirect = new URL(data.redirectUri);
  redirect.searchParams.set('code', code);
  redirect.searchParams.set('state', data.state);
  res.writeHead(302, { location: redirect.toString(), 'cache-control': 'no-store' });
  res.end();
}
async function token(req, res) {
  if (rateLimited(req, 'token', 120, 6000, res)) return;
  const p = await form(req);
  const clientId = String(p.client_id || '');
  const redirects = clientRedirects(clientId);
  if (redirects === false) return oauthError(res, 400, 'invalid_client');
  const now = Math.floor(Date.now() / 1000);
  let sub;
  let grantedScope;
  let grant;
  let presentedJti = null;
  if (p.grant_type === 'authorization_code') {
    const code = codes.get(String(p.code || ''));
    codes.delete(String(p.code || ''));
    if (!code || code.until < Date.now() || code.clientId !== clientId ||
        code.redirectUri !== p.redirect_uri || !/^[A-Za-z0-9._~-]{43,128}$/.test(p.code_verifier || '') ||
        !constantEq(crypto.createHash('sha256').update(p.code_verifier).digest('base64url'), code.challenge)) {
      return oauthError(res, 400, 'invalid_grant');
    }
    sub = crypto.createHash('sha256').update(clientId).digest('hex').slice(0, 24);
    grantedScope = code.grantedScope;
    try {
      grant = authStore.createGrant(clientId, code.ttlHours, code.approvedAtMs,
        code.approvedGeneration);
    } catch (error) {
      if (error.message === 'OAuth authorization code was revoked before exchange') {
        return oauthError(res, 400, 'invalid_grant');
      }
      throw error;
    }
  } else if (p.grant_type === 'refresh_token') {
    const refresh = verify(String(p.refresh_token || ''));
    if (!refresh || refresh.kind !== 'refresh' || refresh.aud !== resource || refresh.iss !== issuer ||
        refresh.clientId !== clientId || !refresh.scope?.split(/\s+/).includes(scope)) return oauthError(res, 400, 'invalid_grant');
    grant = authStore.activeGrant(refresh.gid, clientId);
    if (!grant) return oauthError(res, 400, 'invalid_grant');
    presentedJti = refresh.jti ?? null;
    sub = refresh.sub;
    grantedScope = refresh.scope;
  } else return oauthError(res, 400, 'unsupported_grant_type');
  const grantExp = authStore.expiresAt(grant);
  if (grantExp !== null && grantExp <= now) return oauthError(res, 400, 'invalid_grant');
  const gid = grant.id === 'legacy' ? undefined : grant.id;
  // Deriving, not storing, the successor JTI permits safe short-term retries
  // without retaining any reusable bearer token on disk.
  const nextJti = crypto.createHmac('sha256', signingKey)
    .update('chatx-refresh-v2:')
    .update(JSON.stringify([gid ?? null, presentedJti ?? null, clientId]))
    .digest('base64url');
  let rotation;
  try {
    rotation = authStore.advanceRefresh(gid, presentedJti, nextJti, now);
  } catch (error) {
    if (error.message === 'invalid_grant') return oauthError(res, 400, 'invalid_grant');
    throw error;
  }
  const accessExp = Math.min(rotation.issuedAt + 3600, grantExp ?? Infinity);
  const refreshExp = grantExp === null ? null
    : Math.min(rotation.issuedAt + 30 * 86400, grantExp);
  if (accessExp <= now || (refreshExp !== null && refreshExp <= now)) {
    return oauthError(res, 400, 'invalid_grant');
  }
  const access = sign({ kind: 'access', iss: issuer, aud: resource, scope: grantedScope, sub, gid, exp: accessExp });
  const refresh = sign({ kind: 'refresh', iss: issuer, aud: resource,
    scope: grantedScope, sub, clientId, gid, jti: nextJti, exp: refreshExp });
  json(res, 200, { access_token: access, token_type: 'Bearer', expires_in: accessExp - now,
    refresh_token: refresh, scope: grantedScope });
}
async function handleMcp(req, res) {
  const match = /^Bearer (\S+)$/i.exec(req.headers.authorization || '');
  const claim = match && verify(match[1]);
  if (!claim || claim.kind !== 'access' || claim.iss !== issuer ||
      claim.aud !== resource || !claim.scope?.split(/\s+/).includes(scope) ||
      !authStore.activeGrant(claim.gid)) {
    return challenge(res);
  }
  if (req.method === 'GET') return text(res, 405, 'SSE not supported', { Allow: 'POST' });
  if (req.method === 'DELETE') return text(res, 405, 'Stateless MCP has no sessions', { Allow: 'POST' });
  if (req.method !== 'POST') return text(res, 405, 'Method Not Allowed', { Allow: 'POST' });
  const lengthHeader = req.headers['content-length'];
  const declared = lengthHeader === undefined ? null : Number(lengthHeader);
  const shortBody = Number.isSafeInteger(declared) && declared >= 0 && declared <= 4096;
  let budget = requestBudget.open({ controlCandidate: shortBody });
  // Chunked ping/cancel requests can use the control lane when ordinary body
  // readers are full, but receive a strict 4 KiB / 10-second body limit.
  if (!budget && lengthHeader === undefined) {
    budget = requestBudget.open({ controlCandidate: true });
  }
  if (!budget) {
    return text(res, 429, 'MCP Bridge busy', { 'Retry-After': '1' });
  }
  // HTTP cancellation does not automatically abort a stdio MCP request.
  // Forward it through the MCP SDK so cooperative tools can stop early.
  const upstreamAbort = new AbortController();
  let registeredKey = null;
  let bodyDeadline = budget.controlCandidate
    ? setTimeout(() => req.destroy(), 10_000) : null;
  bodyDeadline?.unref();
  const disconnected = () => {
    if (!res.writableFinished && !upstreamAbort.signal.aborted) {
      upstreamAbort.abort(new Error('MCP HTTP client disconnected'));
    }
  };
  res.once('close', disconnected);
  req.once('aborted', disconnected);
  try {
  const incomingOrigin = req.headers.origin;
  if (incomingOrigin && ![issuer, 'https://chatgpt.com', 'https://chat.openai.com'].includes(incomingOrigin)) {
    return text(res, 403, 'Invalid origin');
  }
  let request;
  try {
    request = JSON.parse(await readBody(req, budget.bodyLimit, (bytes) => {
      if (!budget.reserve(bytes)) {
        const error = new Error('MCP incoming payload budget exhausted');
        error.statusCode = 429;
        throw error;
      }
    }));
  } catch (error) {
    if (upstreamAbort.signal.aborted || res.destroyed) return;
    if (error.statusCode === 429) {
      return text(res, 429, 'MCP Bridge payload budget exceeded', { 'Retry-After': '1' });
    }
    if (error.statusCode === 413) return text(res, 413, 'MCP request too large');
    return json(res, 400, { jsonrpc: '2.0', id: null,
      error: { code: -32700, message: 'Parse error' } });
  }
  if (Array.isArray(request) || request?.jsonrpc !== '2.0' || typeof request.method !== 'string') {
    return json(res, 400, { jsonrpc: '2.0', id: request?.id ?? null,
      error: { code: -32600, message: 'Invalid Request' } });
  }
  if (bodyDeadline) { clearTimeout(bodyDeadline); bodyDeadline = null; }
  if (!budget.admit(request.method)) {
    return text(res, 429, 'MCP Bridge execution slots busy', { 'Retry-After': '1' });
  }
  if (!Object.hasOwn(request, 'id')) {
    if (request.method === 'notifications/cancelled') {
      const key = operationKey(claim, request.params?.requestId);
      if (key) activeOperations.get(key)?.abort(
        new Error('MCP client sent notifications/cancelled'));
    }
    res.writeHead(202); res.end(); return;
  }
  if (!operationKey(claim, request.id)) {
    return json(res, 400, { jsonrpc: '2.0', id: null,
      error: { code: -32600, message: 'Invalid request ID' } });
  }
  if (request.method === 'tools/call' || request.method === 'tools/list') {
    registeredKey = operationKey(claim, request.id);
    if (activeOperations.has(registeredKey)) {
      return text(res, 409, 'Duplicate in-flight MCP request ID');
    }
    activeOperations.set(registeredKey, upstreamAbort);
  }
  if (upstreamAbort.signal.aborted || res.destroyed) return;
  try {
    let result;
    const params = request.params || {};
    switch (request.method) {
      case 'initialize':
        result = { protocolVersion: ['2025-11-25', '2025-06-18', '2025-03-26'].includes(params.protocolVersion)
          ? params.protocolVersion : '2025-06-18',
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: 'ChatX HTTPS Desktop Commander', version: appVersion },
        instructions: 'Access to local desktop files and command execution. Confirm impactful operations.' };
        break;
      case 'ping': result = {}; break;
      case 'tools/list': {
        const upstream = await ensureClient();
        const listing = await upstream.listTools(params.cursor ? { cursor: params.cursor } : {},
          { signal: upstreamAbort.signal, timeout: 600_000 });
        result = { ...listing, tools: listing.tools.map((tool) => ({
          ...tool, securitySchemes: [{ type: 'oauth2', scopes: [scope] }]
        })) };
        break;
      }
      case 'tools/call': {
        if (!params.name || typeof params.name !== 'string') throw Object.assign(new Error('Missing tool name'), { code: -32602 });
        const upstream = await ensureClient();
        result = await upstream.callTool(
          { name: params.name, arguments: params.arguments || {} },
          { signal: upstreamAbort.signal, timeout: 600_000 }
        );
        break;
      }
      default: throw Object.assign(new Error('Method not found'), { code: -32601 });
    }
    if (upstreamAbort.signal.aborted || res.destroyed) return;
    const payload = JSON.stringify({ jsonrpc: '2.0', id: request.id, result });
    if (!budget.reserveResponse(Buffer.byteLength(payload))) {
      return json(res, 200, { jsonrpc: '2.0', id: request.id,
        error: { code: -32603, message: 'MCP response exceeds local buffering budget' } });
    }
    // Keep serialized-response bytes accounted until Node has flushed them,
    // rather than releasing memory credits as soon as res.end() is called.
    res.once('finish', () => budget.releaseResponse());
    res.once('close', () => budget.releaseResponse());
    res.writeHead(200, { 'content-type': 'application/json; charset=utf-8',
      'cache-control': 'no-store', 'x-content-type-options': 'nosniff' });
    res.end(payload);
  } catch (error) {
    if (res.destroyed) return;
    if (upstreamAbort.signal.aborted) {
      return json(res, 200, { jsonrpc: '2.0', id: request.id,
        error: { code: -32800, message: 'MCP request cancelled' } });
    }
    const code = Number.isInteger(error.code) && error.code < 0 ? error.code : -32603;
    json(res, 200, { jsonrpc: '2.0', id: request.id, error: { code, message: code === -32603
      ? 'Desktop Commander unavailable or failed' : String(error.message) } });
    if (code === -32603) console.error('MCP backend request failed:', error.message);
  }
  } finally {
    if (bodyDeadline) clearTimeout(bodyDeadline);
    if (registeredKey && activeOperations.get(registeredKey) === upstreamAbort) {
      activeOperations.delete(registeredKey);
    }
    res.off('close', disconnected);
    req.off('aborted', disconnected);
    budget.close();
  }
}
const server = http.createServer(async (req, res) => {
  try {
    const hostname = (req.headers.host || '').split(':')[0].toLowerCase();
    if (!['127.0.0.1', 'localhost'].includes(hostname) &&
        hostname !== new URL(issuer).hostname) return text(res, 403, 'Invalid host');
    const url = new URL(req.url || '/', issuer);
    if (url.pathname === '/healthz' && req.method === 'GET') {
      const trusted = instanceNonce && constantEq(req.headers['x-chatx-instance-nonce'] || '', instanceNonce);
      return json(res, 200, {
        status: client ? 'ready' : 'starting',
        ...(trusted ? { instance: instanceNonce } : {})
      });
    }
    if (url.pathname === '/.well-known/oauth-protected-resource' ||
        url.pathname === '/.well-known/oauth-protected-resource/mcp') {
      return json(res, 200, resourceMetadata);
    }
    if (url.pathname === '/.well-known/oauth-authorization-server') return json(res, 200, authMetadata);
    if (url.pathname === '/chatx-oauth/register' && req.method === 'POST') {
      if (rateLimited(req, 'register', 10, 1200, res)) return;
      const p = JSON.parse(await readBody(req, 32 * 1024));
      if (!Array.isArray(p.redirect_uris) || p.redirect_uris.length < 1 ||
          p.redirect_uris.length > 5 || !p.redirect_uris.every(validRedirect) ||
          (p.token_endpoint_auth_method && p.token_endpoint_auth_method !== 'none')) {
        return oauthError(res, 400, 'invalid_client_metadata');
      }
      const id = 'chatx.' + sign({ kind: 'client', redirects: p.redirect_uris,
        exp: null });
      return json(res, 201, { client_id: id, client_id_issued_at: Math.floor(Date.now() / 1000),
        token_endpoint_auth_method: 'none', redirect_uris: p.redirect_uris,
        grant_types: ['authorization_code', 'refresh_token'], response_types: ['code'] });
    }
    if (url.pathname === '/chatx-oauth/authorize' && req.method === 'GET') return authorizePage(req, res, url);
    if (url.pathname === '/chatx-oauth/approve' && req.method === 'POST') return await approve(req, res);
    if (url.pathname === '/chatx-oauth/token' && req.method === 'POST') return await token(req, res);
    if (url.pathname === '/mcp') return await handleMcp(req, res);
    text(res, 404, 'Not found');
  } catch (error) {
    console.error('ChatX HTTPS request error:', error.message);
    if (!res.headersSent) text(res, 500, 'Internal server error');
    else res.end();
  }
});
// The WSS relay allows 605s for a request; keep the local upload deadline
// aligned with the 600s desktop HTTP client (including slow 64 MiB uploads).
server.requestTimeout = 600_000;
server.headersTimeout = 20_000;
server.listen(port, '127.0.0.1', () => {
  console.log(`ChatX HTTPS MCP local bridge ready: 127.0.0.1:${port}`);
  ensureClient().catch((error) => console.error('Desktop Commander bootstrap:', error.message));
});
const terminate = async () => {
  server.close();
  try { await transport?.close(); } catch { /* closing */ }
  process.exit(0);
};
process.on('SIGINT', terminate);
process.on('SIGTERM', terminate);
