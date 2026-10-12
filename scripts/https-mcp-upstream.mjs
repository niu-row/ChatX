import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

// One owner for the stdio process and its reconnecting MCP client.
export function createStdioUpstream({ node, launcher, home, entry }) {
  let client;
  let transport;
  let connecting;

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

  async function close() {
    try { await transport?.close(); } catch { /* closing */ }
    transport = null;
    client = null;
  }
  return { ensureClient, close, ready: () => Boolean(client) };
}