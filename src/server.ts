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
import { recordInvocation } from './invocation-log.js';

export const SERVER_NAME = 'chatx';
export const SERVER_VERSION = '0.1.0';

export function buildServer(): McpServer {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    { capabilities: { tools: {} } },
  );

  const registerable = server as unknown as { registerTool: (...args: unknown[]) => unknown };
  const rawRegisterTool = registerable.registerTool.bind(server);
  registerable.registerTool = (...args: unknown[]): unknown => {
    const toolName = typeof args[0] === 'string' ? args[0] : 'unknown';
    const handlerIndex = args.length - 1;
    const handler = args[handlerIndex];
    if (typeof handler !== 'function') return rawRegisterTool(...args);

    args[handlerIndex] = async (...handlerArgs: unknown[]) => {
      const started = Date.now();
      let status: 'ok' | 'error' = 'ok';
      try {
        const result = await handler(...handlerArgs);
        if (result && typeof result === 'object' && 'isError' in result && result.isError === true) status = 'error';
        return result;
      } catch (error) {
        status = 'error';
        throw error;
      } finally {
        recordInvocation({
          startedAt: new Date(started).toISOString(),
          tool: toolName,
          status,
          durationMs: Math.max(0, Date.now() - started),
        });
      }
    };
    return rawRegisterTool(...args);
  };

  server.registerTool(
    'server_info',
    {
      title: 'ChatX server info',
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
          'run_command executes with the permissions of the OS account running ChatX. Filesystem root policy does not sandbox shell commands. git_run additionally requires the Advanced Git permission.',
      });
    },
  );

  registerFilesystemTools(server);
  registerShellTools(server);
  registerGitTools(server);
  return server;
}
