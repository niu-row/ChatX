import os from 'node:os';
import process from 'node:process';
import { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from './config.js';
import { describePathPolicy } from './security/path-policy.js';
import { registerFilesystemTools } from './tools/filesystem.js';
import { registerGitTools } from './tools/git.js';
import { registerShellTools } from './tools/shell.js';
import { textResult } from './utils/results.js';

export const SERVER_NAME = 'chatgptx';
export const SERVER_VERSION = '0.1.0';

export function buildServer(): McpServer {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    { capabilities: { tools: {} } },
  );

  server.registerTool(
    'server_info',
    {
      title: 'ChatGPTX server info',
      description: 'Return server, platform, capability, and local path-policy information.',
      inputSchema: z.object({}),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async () =>
      textResult({
        name: SERVER_NAME,
        version: SERVER_VERSION,
        platform: process.platform,
        arch: process.arch,
        hostname: os.hostname(),
        node_version: process.version,
        cwd: process.cwd(),
        filesystem: describePathPolicy(),
        shell_enabled: config.enableShell,
        warning:
          'run_command executes with the permissions of the OS account running ChatGPTX. Filesystem root policy does not sandbox shell commands.',
      }),
  );

  registerFilesystemTools(server);
  registerShellTools(server);
  registerGitTools(server);
  return server;
}
