import os from 'node:os';
import process from 'node:process';
import { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { getRuntimeSettings, onPermissionSettingsChanged, type PermissionSettings } from './settings.js';
import { describePathPolicy } from './security/path-policy.js';
import { credentialStoreInfo } from './security/credential-store.js';
import { registerFilesystemTools } from './tools/filesystem.js';
import { registerGitTools } from './tools/git.js';
import { registerProjectTools } from './tools/project.js';
import { registerShellTools } from './tools/shell.js';
import { textResult } from './utils/results.js';
import { recordInvocation } from './invocation-log.js';

export const SERVER_NAME = 'chatx';
export const SERVER_VERSION = '0.2.0';

function resultPayloadBytes(result: unknown): number | null {
  if (!result || typeof result !== 'object' || !('content' in result)) return null;
  const content = (result as { content?: unknown }).content;
  if (!Array.isArray(content)) return null;
  let bytes = 0;
  for (const item of content) {
    if (item && typeof item === 'object' && 'text' in item && typeof item.text === 'string') {
      bytes += Buffer.byteLength(item.text);
    } else {
      bytes += Buffer.byteLength(JSON.stringify(item));
    }
  }
  return bytes;
}

export function buildServer(): McpServer {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    { capabilities: { tools: {} } },
  );

  type ToolControl = { enabled?: boolean; enable: () => void; disable: () => void };
  const toolControls = new Map<string, ToolControl>();
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
      let resultBytes: number | null = null;
      try {
        const result = await handler(...handlerArgs);
        resultBytes = resultPayloadBytes(result);
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
          resultBytes,
        });
      }
    };
    const registered = rawRegisterTool(...args);
    if (
      registered &&
      typeof registered === 'object' &&
      'enable' in registered &&
      'disable' in registered
    ) {
      toolControls.set(toolName, registered as ToolControl);
    }
    return registered;
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
  registerProjectTools(server);
  registerShellTools(server);
  registerGitTools(server);

  const filesystemReadTools = new Set([
    'fs_list', 'fs_stat', 'fs_read', 'fs_read_many', 'fs_search', 'fs_project_snapshot',
  ]);
  const filesystemWriteTools = new Set([
    'fs_write', 'fs_append', 'fs_edit', 'fs_mkdir', 'fs_delete', 'fs_move', 'fs_copy',
  ]);
  const shellTools = new Set([
    'run_command', 'run_process', 'process_output', 'process_list', 'process_stdin', 'process_terminate',
  ]);
  const gitReadTools = new Set(['git_status', 'git_diff', 'git_diff_summary', 'git_log', 'git_inspect']);
  const gitWriteTools = new Set(['git_stage', 'git_unstage', 'git_create_branch', 'git_commit']);

  const toolAllowed = (name: string, permissions: PermissionSettings): boolean => {
    if (filesystemReadTools.has(name)) return permissions.filesystemRead;
    if (filesystemWriteTools.has(name)) return permissions.filesystemWrite;
    if (shellTools.has(name)) return permissions.shell;
    if (gitReadTools.has(name)) return permissions.gitRead;
    if (gitWriteTools.has(name)) return permissions.gitWrite;
    if (name === 'git_run') return permissions.gitAdvanced && permissions.gitRead && permissions.gitWrite;
    return true;
  };

  const refreshToolVisibility = () => {
    const permissions = getRuntimeSettings().permissions;
    for (const [name, control] of toolControls) {
      const shouldEnable = toolAllowed(name, permissions);
      if (control.enabled === shouldEnable) continue;
      if (shouldEnable) control.enable();
      else control.disable();
    }
  };

  refreshToolVisibility();
  const unsubscribePermissions = onPermissionSettingsChanged(refreshToolVisibility);
  const previousOnClose = server.server.onclose;
  server.server.onclose = () => {
    unsubscribePermissions();
    previousOnClose?.();
  };
  return server;
}
