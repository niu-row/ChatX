import { createServer } from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createMcpHandler } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import { localhostHostValidation, localhostOriginValidation, toNodeHandler } from '@modelcontextprotocol/node';
import { config } from './config.js';
import { buildServer, SERVER_NAME, SERVER_VERSION } from './server.js';
import { TunnelDashboard } from './dashboard.js';

function tokenMatches(header: string | undefined): boolean {
  if (!config.authToken) return true;
  if (!header?.startsWith('Bearer ')) return false;
  const supplied = Buffer.from(header.slice('Bearer '.length), 'utf8');
  const expected = Buffer.from(config.authToken, 'utf8');
  return supplied.length === expected.length && timingSafeEqual(supplied, expected);
}

function isLoopbackHost(host: string): boolean {
  return host === '127.0.0.1' || host === 'localhost' || host === '::1';
}

function reportMcpError(error: unknown): void {
  const message = error instanceof Error ? error.message : String(error);
  if (message.includes('Unsupported Media Type: Content-Type must be application/json')) return;
  console.error(`[${SERVER_NAME}] MCP error:`, error);
}

if (process.argv.includes('--stdio')) {
  serveStdio(buildServer);
  console.error(`[${SERVER_NAME}] ${SERVER_VERSION} serving MCP over stdio`);
} else {
  const dashboard = new TunnelDashboard();
  const handler = createMcpHandler(buildServer, {
    onerror: reportMcpError,
  });
  const nodeHandler = toNodeHandler(handler, {
    onerror: (error) => console.error(`[${SERVER_NAME}] HTTP adapter error:`, error),
  });

  const validateHost = isLoopbackHost(config.host) ? localhostHostValidation() : null;
  const validateOrigin = isLoopbackHost(config.host) ? localhostOriginValidation() : null;

  const httpServer = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', `http://${req.headers.host ?? 'localhost'}`);

    if (url.pathname === '/healthz' || url.pathname === '/readyz') {
      res.writeHead(200, { 'content-type': 'application/json; charset=utf-8' });
      res.end(
        JSON.stringify({
          ok: true,
          service: SERVER_NAME,
          version: SERVER_VERSION,
          mcp_endpoint: '/mcp',
        }),
      );
      return;
    }

    if (isLoopbackHost(config.host)) {
      if (validateHost && !validateHost(req, res)) return;
      if (req.method === 'POST' && validateOrigin && !validateOrigin(req, res)) return;
      if (await dashboard.handle(req, res, url.pathname)) return;
    }

    if (url.pathname !== '/mcp') {
      res.writeHead(404, { 'content-type': 'application/json; charset=utf-8' });
      res.end(JSON.stringify({ error: 'not_found' }));
      return;
    }

    if (!tokenMatches(req.headers.authorization)) {
      res.writeHead(401, {
        'content-type': 'application/json; charset=utf-8',
        'www-authenticate': 'Bearer',
      });
      res.end(JSON.stringify({ error: 'unauthorized' }));
      return;
    }

    if (validateHost && !validateHost(req, res)) return;
    if (validateOrigin && !validateOrigin(req, res)) return;

    if (req.method === 'POST') {
      const contentType = String(req.headers['content-type'] ?? '').toLowerCase();
      const mediaType = contentType.split(';', 1)[0]?.trim() ?? '';
      if (mediaType !== 'application/json') {
        const userAgent = String(req.headers['user-agent'] ?? 'unknown');
        console.warn(`[${SERVER_NAME}] rejected non-JSON MCP POST (content-type=${contentType || 'missing'}, user-agent=${userAgent})`);
        res.writeHead(415, { 'content-type': 'application/json; charset=utf-8' });
        res.end(
          JSON.stringify({
            jsonrpc: '2.0',
            error: { code: -32000, message: 'Unsupported Media Type: Content-Type must be application/json' },
            id: null,
          }),
        );
        return;
      }
    }

    void nodeHandler(req, res);
  });

  httpServer.listen(config.port, config.host, () => {
    console.error(`[${SERVER_NAME}] ${SERVER_VERSION} listening on http://${config.host}:${config.port}/mcp`);
    console.error(`[${SERVER_NAME}] health: http://${config.host}:${config.port}/healthz`);
    if (isLoopbackHost(config.host)) {
      console.error(`[${SERVER_NAME}] console: http://${config.host}:${config.port}/`);
      if (process.argv.includes('--open-ui')) {
        const url = `http://${config.host}:${config.port}/`;
        if (process.platform === 'win32') {
          const child = spawn('cmd.exe', ['/d', '/s', '/c', 'start', '', url], {
            detached: true,
            stdio: 'ignore',
            windowsHide: true,
          });
          child.unref();
        }
      }
    }
    if (!isLoopbackHost(config.host)) {
      console.error(
        `[${SERVER_NAME}] WARNING: non-loopback bind (${config.host}). Use network controls and CHATGPTX_AUTH_TOKEN.`,
      );
    }
  });

  const shutdown = async (signal: string) => {
    console.error(`[${SERVER_NAME}] received ${signal}, shutting down`);
    dashboard.close();
    httpServer.close();
    await handler.close().catch(() => {});
    process.exit(0);
  };

  process.once('SIGINT', () => void shutdown('SIGINT'));
  process.once('SIGTERM', () => void shutdown('SIGTERM'));
}
