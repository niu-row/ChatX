import os from 'node:os';
import process from 'node:process';
import { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { getRuntimeSettings } from './settings.js';
import { describePathPolicy } from './security/path-policy.js';
import { credentialStoreInfo } from './security/credential-store.js';
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
      description: 'Return server, platform, capability, local path-policy, and runtime settings information.',
      inputSchema: z.object({}),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async () => {
      const runtime = getRuntimeSettings();
      const credentials = credentialStoreInfo();
      return textResult({
        name: SERVER_NAME,
        version: SERVER_VERSION,
        platform: process.platform,
        arch: process.arch,
        hostname: os.hostname(),
        node_version: process.version,
        cwd: process.cwd(),
        settings_version: runtime.version,
        permission_preset: runtime.permissionPreset,
        filesystem: describePathPolicy(),
        shell_enabled: runtime.permissions.shell,
        permissions: runtime.permissions,
        runtime_key_store: {
          supported: credentials.supported,
          provider: credentials.provider,
          saved: credentials.saved,
        },
        warning:
          'run_command executes with the permissions of the OS account running ChatGPTX. Filesystem root policy does not sandbox shell commands. git_run additionally requires the Advanced Git permission.',
      });
    },
  );

  registerFilesystemTools(server);
  registerShellTools(server);
  registerGitTools(server);
  return server;
}
