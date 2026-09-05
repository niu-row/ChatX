import { createServer } from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import { createMcpHandler } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import { localhostHostValidation, localhostOriginValidation, toNodeHandler } from '@modelcontextprotocol/node';
import { config } from './config.js';
import { buildServer, SERVER_NAME, SERVER_VERSION } from './server.js';

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

if (process.argv.includes('--stdio')) {
  serveStdio(buildServer);
  console.error(`[${SERVER_NAME}] ${SERVER_VERSION} serving MCP over stdio`);
} else {
  const handler = createMcpHandler(buildServer, {
    onerror: (error) => console.error(`[${SERVER_NAME}] MCP error:`, error),
  });
  const nodeHandler = toNodeHandler(handler, {
    onerror: (error) => console.error(`[${SERVER_NAME}] HTTP adapter error:`, error),
  });

  const validateHost = isLoopbackHost(config.host) ? localhostHostValidation() : null;
  const validateOrigin = isLoopbackHost(config.host) ? localhostOriginValidation() : null;

  const httpServer = createServer((req, res) => {
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

    void nodeHandler(req, res);
  });

  httpServer.listen(config.port, config.host, () => {
    console.error(`[${SERVER_NAME}] ${SERVER_VERSION} listening on http://${config.host}:${config.port}/mcp`);
    console.error(`[${SERVER_NAME}] health: http://${config.host}:${config.port}/healthz`);
    if (!isLoopbackHost(config.host)) {
      console.error(
        `[${SERVER_NAME}] WARNING: non-loopback bind (${config.host}). Use network controls and CHATGPTX_AUTH_TOKEN.`,
      );
    }
  });

  const shutdown = async (signal: string) => {
    console.error(`[${SERVER_NAME}] received ${signal}, shutting down`);
    httpServer.close();
    await handler.close().catch(() => {});
    process.exit(0);
  };

  process.once('SIGINT', () => void shutdown('SIGINT'));
  process.once('SIGTERM', () => void shutdown('SIGTERM'));
}
