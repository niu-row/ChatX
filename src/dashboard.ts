import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { config } from './config.js';
import {
  SETTINGS_VERSION,
  getRuntimeSettings,
  permissionMetadata,
  saveTunnelId,
  settingsFilePath,
  updateRuntimeSettings,
} from './settings.js';
import { clearRuntimeKey, credentialStoreInfo, loadRuntimeKey, saveRuntimeKey } from './security/credential-store.js';
import { SERVER_NAME, SERVER_VERSION } from './server.js';
import { terminateAllManagedProcesses } from './tools/shell.js';
import { DASHBOARD_HTML } from './dashboard-page.js';
import { getInvocationLog, getInvocationSummary } from './invocation-log.js';

const PROFILE_NAME = 'chatx';
const MAX_BODY_BYTES = 32 * 1024;
const MAX_LOG_LINES = 300;
const DISCOVERY_CONTENT_TYPE_HEADER = 'Content-Type: application/json';

type CommandResult = {
  ok: boolean;
  exitCode: number | null;
  output: string;
};

type DiagnosticCheck = {
  name: string;
  status: 'ok' | 'warn' | 'error';
  message: string;
};

function json(res: ServerResponse, status: number, value: unknown): void {
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff',
  });
  res.end(JSON.stringify(value));
}

function text(res: ServerResponse, status: number, value: string, contentType: string): void {
  res.writeHead(status, {
    'content-type': contentType,
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff',
    'content-security-policy':
      "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; " +
      "connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
    'referrer-policy': 'no-referrer',
  });
  res.end(value);
}

async function readJson(req: IncomingMessage): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += buffer.length;
    if (size > MAX_BODY_BYTES) throw new Error('请求内容过大。');
    chunks.push(buffer);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}') as Record<string, unknown>;
  } catch {
    throw new Error('请求格式无效。');
  }
}

function tunnelExecutable(): string {
  const configured = process.env.TUNNEL_CLIENT_PATH?.trim();
  if (configured) return configured;
  if (process.platform === 'win32') {
    const localAppData = process.env.LOCALAPPDATA;
    if (localAppData) {
      const installed = path.join(localAppData, 'Programs', 'OpenAI', 'tunnel-client', 'tunnel-client.exe');
      if (fs.existsSync(installed)) return installed;
    }
  }
  return 'tunnel-client';
}

function tunnelVersion(executable: string): string | null {
  const result = spawnSync(executable, ['--version'], {
    encoding: 'utf8',
    windowsHide: true,
    timeout: 5_000,
  });
  if (result.error || result.status !== 0) return null;
  return (result.stdout || result.stderr).trim() || null;
}

function maskTunnelId(value: string): string {
  if (value.length < 18) return value;
  return `${value.slice(0, 11)}…${value.slice(-6)}`;
}

function hostForUrl(host: string): string {
  return host === '::1' ? '[::1]' : host;
}

function localBaseUrl(): string {
  return `http://${hostForUrl(config.host)}:${config.port}`;
}

function localMutationAllowed(req: IncomingMessage): boolean {
  return Boolean(req.headers.origin) && req.headers['sec-fetch-site'] !== 'cross-site';
}

function isJsonRequest(req: IncomingMessage): boolean {
  return req.headers['content-type']?.toLowerCase().startsWith('application/json') ?? false;
}

function isLoopbackUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (
      url.protocol === 'http:' &&
      ['127.0.0.1', 'localhost', '::1', '[::1]'].includes(url.hostname.toLowerCase())
    );
  } catch {
    return false;
  }
}

async function fetchOk(url: string, timeoutMs = 2_000): Promise<{ ok: boolean; status: number; text: string }> {
  try {
    const response = await fetch(url, { cache: 'no-store', signal: AbortSignal.timeout(timeoutMs) });
    return { ok: response.ok, status: response.status, text: await response.text() };
  } catch (error) {
    return { ok: false, status: 0, text: error instanceof Error ? error.message : String(error) };
  }
}

export class TunnelDashboard {
  private readonly executable = tunnelExecutable();
  private readonly version = tunnelVersion(this.executable);
  private readonly healthUrlFile = path.join(config.settingsDir, 'tunnel-health-url.txt');
  private child: ChildProcess | null = null;
  private logs: string[] = [];
  private operation: 'connecting' | 'stopping' | null = null;
  private lastTunnelId: string | null = getRuntimeSettings().connection.tunnelId;
  private lastError: string | null = null;
  private tunnelHealthUrl: string | null = null;

  private addLog(source: string, value: string): void {
    const lines = value.replace(/\r/g, '').split('\n').filter(Boolean);
    const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
    for (const line of lines) this.logs.push(`[${time}] ${source}  ${line}`);
    if (this.logs.length > MAX_LOG_LINES) this.logs.splice(0, this.logs.length - MAX_LOG_LINES);
  }

  private snapshot() {
    const runtime = getRuntimeSettings();
    const credential = credentialStoreInfo();
    return {
      settings: { version: runtime.version },
      service: { name: SERVER_NAME, version: SERVER_VERSION, endpoint: `${localBaseUrl()}/mcp` },
      policy: {
        roots: runtime.filesystem.roots,
        fullAccess: runtime.permissions.fullAccess,
        shellEnabled: runtime.permissions.shell,
        permissionPreset: runtime.permissionPreset,
        permissions: runtime.permissions,
        permissionMetadata,
      },
      connection: {
        tunnelId: runtime.connection.tunnelId,
        runtimeKeySaved: credential.saved,
        runtimeKeySupported: credential.supported,
        runtimeKeyProvider: credential.provider,
        settingsFile: settingsFilePath(),
      },
      tunnel: {
        installed: this.version !== null,
        version: this.version,
        profile: PROFILE_NAME,
        state: this.operation ?? (this.child && this.child.exitCode === null ? 'running' : 'stopped'),
        tunnelId: this.lastTunnelId ? maskTunnelId(this.lastTunnelId) : null,
        healthUrl: this.tunnelHealthUrl,
        lastError: this.lastError,
      },
      logs: this.logs.slice(-120),
    };
  }

  private run(args: string[], apiKey: string, timeoutMs: number): Promise<CommandResult> {
    return new Promise((resolve) => {
      const child = spawn(this.executable, args, {
        env: { ...process.env, CONTROL_PLANE_API_KEY: apiKey },
        stdio: ['ignore', 'pipe', 'pipe'],
        windowsHide: true,
      });
      let output = '';
      let settled = false;
      const append = (chunk: Buffer, source: string) => {
        const safe = chunk.toString('utf8').split(apiKey).join('[redacted]');
        output += safe;
        if (output.length > 200_000) output = output.slice(-200_000);
        this.addLog(source, safe);
      };
      child.stdout.on('data', (chunk: Buffer) => append(chunk, 'tunnel'));
      child.stderr.on('data', (chunk: Buffer) => append(chunk, 'tunnel'));
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        child.kill();
        resolve({ ok: false, exitCode: null, output: `${output}\n操作超时。` });
      }, timeoutMs);
      child.once('error', (error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve({ ok: false, exitCode: null, output: `${output}\n${error.message}` });
      });
      child.once('exit', (code) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve({ ok: code === 0, exitCode: code, output: output.trim() });
      });
    });
  }

  private async readTunnelHealthUrl(): Promise<string | null> {
    try {
      const value = fs.readFileSync(this.healthUrlFile, 'utf8').trim().replace(/\/$/, '');
      return value && isLoopbackUrl(value) ? value : null;
    } catch {
      return null;
    }
  }

  private async probeTunnelReady(): Promise<{ ok: boolean; message: string }> {
    const healthUrl = this.tunnelHealthUrl ?? (await this.readTunnelHealthUrl());
    if (!healthUrl) return { ok: false, message: '尚未获得 tunnel-client health URL' };
    this.tunnelHealthUrl = healthUrl;
    const ready = await fetchOk(`${healthUrl}/readyz`, 2_000);
    if (!ready.ok) return { ok: false, message: `readyz 未就绪（HTTP ${ready.status || '连接失败'}）` };
    return { ok: true, message: `readyz 正常：${healthUrl}` };
  }

  private async probeLocalMcp(): Promise<{ ok: boolean; message: string }> {
    const result = await fetchOk(`${localBaseUrl()}/healthz`, 2_000);
    if (!result.ok) return { ok: false, message: `本地 MCP healthz 失败（HTTP ${result.status || '连接失败'}）` };
    try {
      const body = JSON.parse(result.text) as { ok?: boolean; service?: string };
      if (body.ok !== true || body.service !== SERVER_NAME) return { ok: false, message: '本地 healthz 返回内容异常' };
    } catch {
      return { ok: false, message: '本地 healthz 不是有效 JSON' };
    }
    return { ok: true, message: `${localBaseUrl()}/healthz` };
  }

  private async start(apiKey: string): Promise<CommandResult> {
    fs.mkdirSync(path.dirname(this.healthUrlFile), { recursive: true });
      fs.rmSync(this.healthUrlFile, { force: true });
      this.tunnelHealthUrl = null;

      const child = spawn(
        this.executable,
        [
          'run',
          '--profile', PROFILE_NAME,
          '--health.url-file', this.healthUrlFile,
          '--mcp.discovery-extra-headers', DISCOVERY_CONTENT_TYPE_HEADER,
        ],
        {
          env: { ...process.env, CONTROL_PLANE_API_KEY: apiKey },
          stdio: ['ignore', 'pipe', 'pipe'],
          windowsHide: true,
        },
      );
      this.child = child;
      let startupOutput = '';
      let spawnError: string | null = null;
      const append = (chunk: Buffer) => {
        const safe = chunk.toString('utf8').split(apiKey).join('[redacted]');
        startupOutput += safe;
        if (startupOutput.length > 200_000) startupOutput = startupOutput.slice(-200_000);
        this.addLog('runtime', safe);
      };
      child.stdout.on('data', append);
      child.stderr.on('data', append);
      child.once('error', (error) => {
        spawnError = error.message;
        this.lastError = error.message;
      });
      child.once('exit', (code) => {
        this.addLog('runtime', `进程已退出，代码 ${code ?? 'unknown'}。`);
        if (code !== 0 && this.operation !== 'stopping') {
          this.lastError = startupOutput.trim() || spawnError || `Tunnel 进程退出，代码 ${code ?? 'unknown'}。`;
        }
        if (this.child === child) this.child = null;
      });

      const deadline = Date.now() + 15_000;
      while (Date.now() < deadline) {
        if (spawnError) {
          return { ok: false, exitCode: child.exitCode, output: spawnError };
        }
        if (child.exitCode !== null) {
          return { ok: false, exitCode: child.exitCode, output: startupOutput.trim() };
        }
        this.tunnelHealthUrl = await this.readTunnelHealthUrl();
        if (this.tunnelHealthUrl) {
          const ready = await this.probeTunnelReady();
          if (ready.ok) {
            this.addLog('health', ready.message);
            return { ok: true, exitCode: null, output: startupOutput.trim() };
          }
        }
        await new Promise((done) => setTimeout(done, 300));
      }

      child.kill();
      return { ok: false, exitCode: child.exitCode, output: `${startupOutput.trim()}\nTunnel 启动后 15 秒内未通过 /readyz 健康检查。` };
  }

  private async connect(req: IncomingMessage, res: ServerResponse): Promise<void> {
    if (this.operation) return json(res, 409, { ok: false, error: '已有操作正在进行。' });
    if (this.child && this.child.exitCode === null) return json(res, 409, { ok: false, error: 'Tunnel 已经在运行。' });

    const body = await readJson(req);
    const tunnelId = typeof body.tunnelId === 'string' ? body.tunnelId.trim() : '';
    const suppliedKey = typeof body.apiKey === 'string' ? body.apiKey.trim() : '';
    const rememberKey = body.rememberKey === true;
    if (!/^tunnel_[a-z0-9]{8,128}$/.test(tunnelId)) {
      return json(res, 400, { ok: false, error: 'Tunnel ID 格式不正确。' });
    }
    if (this.version === null) return json(res, 503, { ok: false, error: '没有找到 tunnel-client。' });

    let apiKey = suppliedKey;
    if (!apiKey) {
      try {
        apiKey = loadRuntimeKey() ?? '';
      } catch (error) {
        return json(res, 400, { ok: false, error: `读取已保存 Runtime Key 失败：${error instanceof Error ? error.message : String(error)}` });
      }
    }
    if (apiKey.length < 12) return json(res, 400, { ok: false, error: 'Runtime API Key 不能为空，且当前没有可用的安全保存 Key。' });

    this.operation = 'connecting';
    this.lastError = null;
    this.lastTunnelId = tunnelId;
    try {
      saveTunnelId(tunnelId);
      const local = await this.probeLocalMcp();
      if (!local.ok) throw new Error(local.message);
      this.addLog('health', `本地 MCP 已就绪：${local.message}`);

      const endpoint = `${localBaseUrl()}/mcp`;
      this.addLog('console', '正在创建 Tunnel 配置…');
      const initialized = await this.run(
        [
          'init', '--force', '--sample', 'sample_mcp_remote_no_auth', '--profile', PROFILE_NAME,
          '--tunnel-id', tunnelId, '--mcp-server-url', endpoint, '--health-listen-addr', '127.0.0.1:0',
        ],
        apiKey,
        20_000,
      );
      if (!initialized.ok) throw new Error(initialized.output || '创建 Tunnel 配置失败。');

      this.addLog('console', '正在检查账号权限和 MCP discovery…');
      const diagnosed = await this.run(
        ['doctor', '--profile', PROFILE_NAME, '--explain', '--mcp.discovery-extra-headers', DISCOVERY_CONTENT_TYPE_HEADER],
        apiKey,
        60_000,
      );
      if (!diagnosed.ok) throw new Error(diagnosed.output || 'Tunnel 诊断失败。');

      this.addLog('console', '诊断通过，正在启动 Tunnel 并等待 /readyz…');
      const started = await this.start(apiKey);
      if (!started.ok) throw new Error(started.output || 'Tunnel 启动失败。');

      if (rememberKey && suppliedKey) {
        saveRuntimeKey(suppliedKey);
        this.addLog('security', 'Runtime Key 已使用 Windows DPAPI（CurrentUser）安全保存。');
      }
      this.addLog('console', 'Tunnel 已连接并通过健康检查。');
      json(res, 200, { ok: true, status: this.snapshot() });
    } catch (error) {
      this.lastError = error instanceof Error ? error.message : String(error);
      this.addLog('error', this.lastError);
      if (this.child && this.child.exitCode === null) this.child.kill();
      this.child = null;
      json(res, 400, { ok: false, error: this.lastError, status: this.snapshot() });
    } finally {
      this.operation = null;
    }
  }

  private async updateSettings(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const body = await readJson(req);
    const before = getRuntimeSettings();
    const after = updateRuntimeSettings(body);
    if (before.permissions.shell && !after.permissions.shell) {
      const terminated = await terminateAllManagedProcesses();
      this.addLog('console', `Shell 已关闭；已请求终止 ${terminated} 个由 ChatX 管理的后台进程。`);
    } else {
      this.addLog('console', '设置已更新，并立即对后续 MCP 工具调用生效。');
    }
    json(res, 200, { ok: true, status: this.snapshot() });
  }

  private async diagnostics(): Promise<DiagnosticCheck[]> {
    const checks: DiagnosticCheck[] = [];
    const local = await this.probeLocalMcp();
    checks.push({ name: '本地 MCP', status: local.ok ? 'ok' : 'error', message: local.message });

    checks.push({
      name: 'tunnel-client',
      status: this.version ? 'ok' : 'error',
      message: this.version ?? '未找到可执行文件',
    });

    const runtime = getRuntimeSettings();
    const missingRoots = runtime.filesystem.roots.filter((root) => !fs.existsSync(root));
    checks.push({
      name: '允许目录',
      status: missingRoots.length === 0 ? 'ok' : 'warn',
      message: missingRoots.length === 0 ? `${runtime.filesystem.roots.length} 个目录可用` : `不存在：${missingRoots.join('; ')}`,
    });

    const git = spawnSync('git', ['--version'], { encoding: 'utf8', windowsHide: true, timeout: 5_000 });
    checks.push({
      name: 'Git',
      status: git.status === 0 ? 'ok' : 'warn',
      message: git.status === 0 ? git.stdout.trim() : '未找到 Git 或无法执行',
    });

    const credentials = credentialStoreInfo();
    checks.push({
      name: 'Runtime Key 存储',
      status: credentials.supported ? 'ok' : 'warn',
      message: credentials.supported ? (credentials.saved ? 'Windows DPAPI 已保存' : 'Windows DPAPI 可用，当前未保存') : '当前平台不支持安全持久化',
    });

    if (this.child && this.child.exitCode === null) {
      const tunnel = await this.probeTunnelReady();
      checks.push({ name: 'Tunnel /readyz', status: tunnel.ok ? 'ok' : 'error', message: tunnel.message });
    } else {
      checks.push({ name: 'Tunnel /readyz', status: 'warn', message: 'Tunnel 当前未运行' });
    }

    checks.push({ name: '设置迁移', status: 'ok', message: `settings v${SETTINGS_VERSION}` });
    return checks;
  }

  private async stop(res: ServerResponse): Promise<void> {
    if (!this.child || this.child.exitCode !== null) {
      this.child = null;
      this.tunnelHealthUrl = null;
      return json(res, 200, { ok: true, status: this.snapshot() });
    }
    this.operation = 'stopping';
    this.addLog('console', '正在停止 Tunnel…');
    const child = this.child;
    child.kill();
    await new Promise((resolve) => setTimeout(resolve, 700));
    if (child.exitCode === null) child.kill('SIGKILL');
    this.child = null;
    this.tunnelHealthUrl = null;
    this.operation = null;
    this.addLog('console', 'Tunnel 已停止。');
    json(res, 200, { ok: true, status: this.snapshot() });
  }

  async handle(req: IncomingMessage, res: ServerResponse, pathname: string): Promise<boolean> {
    if (req.method === 'GET' && (pathname === '/' || pathname === '/console')) {
      text(res, 200, DASHBOARD_HTML, 'text/html; charset=utf-8');
      return true;
    }
    if (req.method === 'GET' && pathname === '/api/tunnel/status') {
      json(res, 200, { ok: true, status: this.snapshot() });
      return true;
    }
    if (req.method === 'GET' && pathname === '/api/diagnostics') {
      json(res, 200, { ok: true, checks: await this.diagnostics(), status: this.snapshot() });
      return true;
    }
    if (req.method === 'GET' && pathname === '/api/invocations') {
      json(res, 200, { ok: true, entries: getInvocationLog(500), summary: getInvocationSummary() });
      return true;
    }

    const mutating = pathname === '/api/settings' || pathname === '/api/tunnel/connect' || pathname === '/api/tunnel/stop' || pathname === '/api/tunnel/key/clear';
    if (req.method === 'POST' && mutating) {
      if (!localMutationAllowed(req)) {
        json(res, 403, { ok: false, error: '仅允许从本地控制台执行此操作。' });
        return true;
      }
      if (!isJsonRequest(req)) {
        json(res, 415, { ok: false, error: '请求类型必须是 application/json。' });
        return true;
      }
      try {
        if (pathname === '/api/settings') await this.updateSettings(req, res);
        else if (pathname === '/api/tunnel/connect') await this.connect(req, res);
        else if (pathname === '/api/tunnel/stop') await this.stop(res);
        else {
          clearRuntimeKey();
          this.addLog('security', '已清除本机安全保存的 Runtime Key。');
          json(res, 200, { ok: true, status: this.snapshot() });
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        this.lastError = message;
        this.addLog('error', message);
        json(res, 400, { ok: false, error: message, status: this.snapshot() });
      }
      return true;
    }
    return false;
  }

  close(): void {
    if (this.child && this.child.exitCode === null) this.child.kill();
    this.child = null;
    this.tunnelHealthUrl = null;
  }
}
