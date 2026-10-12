import { createMcpRequestBudget } from './mcp-request-budget.mjs';

// HTTP request accounting, JSON-RPC handling, and cancellation correlation
// stay local to this factory; OAuth routes never touch these mutable maps.
export function createMcpHandler({
  issuer, resource, scope, appVersion, authStore, verify,
  readBody, challenge, text, json, ensureClient,
}) {
  // Eight long-running tools plus four reserved control requests. Accounting
  // also bounds buffered incoming body bytes across all active JSON requests.
  // This is not a cap on Desktop Commander output or V8 heap usage.
  const requestBudget = createMcpRequestBudget();
  // JSON-RPC IDs belong to independent client sessions. A single OAuth
  // grant may have multiple simultaneous HTTP clients using the same ID.
  // Keep all instances; an ambiguous explicit cancel never guesses a target.
  const activeOperations = new Map();
  const RECHECK_AUTHORIZATION_MS = 250;
  function sessionId(req) {
    const value = req.headers['mcp-session-id'];
    return typeof value === 'string' && /^[\x21-\x7e]{1,128}$/.test(value)
      ? value : null;
  }
  function operationKey(claim, id) {
    if (typeof id === 'string' && id.length > 0 && id.length <= 128) {
      return JSON.stringify([claim.gid ?? 'legacy', claim.sub ?? '', 'string', id]);
    }
    if (typeof id === 'number' && Number.isSafeInteger(id)) {
      return JSON.stringify([claim.gid ?? 'legacy', claim.sub ?? '', 'number', id]);
    }
    return null;
  }

  async function handleMcp(req, res) {
    const match = /^Bearer (\S+)$/i.exec(req.headers.authorization || '');
    const claim = match && verify(match[1]);
    if (!claim || claim.kind !== 'access' || claim.iss !== issuer ||
        claim.aud !== resource || !claim.scope?.split(/\s+/).includes(scope) ||
        !authStore.activeGrant(claim.gid)) {
      return challenge(res);
    }
    const grantStillValid = () => {
      try {
        return !!authStore.activeGrant(claim.gid) &&
          Number.isSafeInteger(claim.exp) && claim.exp > Math.floor(Date.now() / 1000);
      } catch { return false; } // Unreadable grant storage fails closed.
    };
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
    let registeredOperation = null;
    let revocationWatch = null;
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
        const group = key && activeOperations.get(key);
        const session = sessionId(req);
        const matches = group ? [...group].filter((operation) =>
          session === null || operation.session === session) : [];
        if (matches.length > 1) {
          // Stateless clients do not always identify their session. With
          // duplicate IDs it is safer to reject cancellation than to stop
          // a different client's local file or shell operation.
          return text(res, 409, 'Ambiguous MCP cancellation: session ID required');
        }
        if (matches.length === 1) matches[0].abort.abort(
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
      registeredOperation = { abort: upstreamAbort, session: sessionId(req) };
      let group = activeOperations.get(registeredKey);
      if (!group) {
        group = new Set();
        activeOperations.set(registeredKey, group);
      }
      group.add(registeredOperation);
      const stillAuthorized = () => !upstreamAbort.signal.aborted && grantStillValid();
      const recheck = () => {
        if (!stillAuthorized()) upstreamAbort.abort(
          new Error('MCP authorization revoked or expired'));
      };
      // OAuth management runs in another process. Poll the persisted grant
      // during execution so admin revoke/revoke-all cancels in-flight work.
      revocationWatch = setInterval(recheck, RECHECK_AUTHORIZATION_MS);
      revocationWatch.unref();
      upstreamAbort.signal.addEventListener('abort', () => {
        if (revocationWatch) clearInterval(revocationWatch);
      }, { once: true });
      recheck();
    }
    if (res.destroyed) return;
    if (upstreamAbort.signal.aborted) {
      return json(res, 200, { jsonrpc: '2.0', id: request.id,
        error: { code: -32800, message: 'MCP request cancelled' } });
    }
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
      if (registeredOperation && !grantStillValid()) {
        upstreamAbort.abort(new Error('MCP authorization revoked or expired'));
      }
      if (res.destroyed) return;
      if (upstreamAbort.signal.aborted) {
        return json(res, 200, { jsonrpc: '2.0', id: request.id,
          error: { code: -32800, message: 'MCP request cancelled' } });
      }
      const payload = JSON.stringify({ jsonrpc: '2.0', id: request.id, result });
      // Stringifying a large result can itself take time. Revalidate right
      // before writing response headers as well as before serialization.
      if (registeredOperation && !grantStillValid()) {
        upstreamAbort.abort(new Error('MCP authorization revoked or expired'));
        return json(res, 200, { jsonrpc: '2.0', id: request.id,
          error: { code: -32800, message: 'MCP request cancelled' } });
      }
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
      if (registeredOperation && !grantStillValid()) {
        upstreamAbort.abort(new Error('MCP authorization revoked or expired'));
      }
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
      if (revocationWatch) clearInterval(revocationWatch);
      if (registeredKey && registeredOperation) {
        const group = activeOperations.get(registeredKey);
        group?.delete(registeredOperation);
        if (group?.size === 0) activeOperations.delete(registeredKey);
      }
      res.off('close', disconnected);
      req.off('aborted', disconnected);
      budget.close();
    }
  }

  return handleMcp;
}