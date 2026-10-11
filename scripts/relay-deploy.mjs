// Bundled Node runner. SSH is used only for server provisioning, not MCP runtime.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const [mode, archivePath] = process.argv.slice(2);
const root = path.dirname(fileURLToPath(import.meta.url));
const emit = (message) => process.stdout.write(
  JSON.stringify({ kind: 'progress', message }) + '\n');
const result = (value) => process.stdout.write(
  JSON.stringify({ kind: 'result', ...value }) + '\n');
function safeError(error) {
  return String(error?.message ?? error).slice(-800)
    .replace(/Bearer\s+[^\s]+/gi, 'Bearer [redacted]')
    .replace(/(TOKEN|PASSWORD|SECRET)=[^\s]+/gi, '$1=[redacted]');
}
function checkHost(host) {
  return /^[a-zA-Z0-9](?:[a-zA-Z0-9.-]{0,251}[a-zA-Z0-9])?$/.test(host)
    && !host.includes('..') && !host.startsWith('-');
}
function locate(name) {
  if (process.platform === 'win32') {
    const git = path.join(process.env.ProgramFiles || 'C:\\Program Files',
      'Git', 'usr', 'bin', name + '.exe');
    if (fs.existsSync(git)) return git;
    const native = path.join(process.env.SystemRoot || 'C:\\Windows',
      'System32', 'OpenSSH', name + '.exe');
    if (fs.existsSync(native)) return native;
  }
  return name;
}
async function exec(binary, args, input, captureLines = false, maxSeconds = 30) {
  return await new Promise((resolve, reject) => {
    let output = '';
    let errors = '';
    let lines = '';
    // Desktop Commander isolates HOME; SSH must read the real login account's
    // known_hosts and key defaults, not the MCP tool sandbox.
    const child = spawn(binary, args, { windowsHide: true,
      env: { ...process.env, HOME: os.userInfo().homedir },
      stdio: ['pipe', 'pipe', 'pipe'] });
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error('SSH timed out; check server state before retrying'));
    }, maxSeconds * 1000);
    const append = (chunk, isError) => {
      const text = chunk.toString('utf8');
      if (isError) errors = (errors + text).slice(-2000);
      else output = (output + text).slice(-8192);
      if (captureLines && !isError) {
        lines += text;
        const complete = lines.split(/\r?\n/);
        lines = complete.pop() ?? '';
        for (const line of complete) {
          if (/^(PREFLIGHT_OK|STAGE=|DEPLOY_OK)/.test(line)) emit(line.slice(0, 180));
        }
      }
    };
    child.stdout.on('data', (chunk) => append(chunk, false));
    child.stderr.on('data', (chunk) => append(chunk, true));
    child.once('error', (error) => { clearTimeout(timeout); reject(error); });
    child.once('close', (code) => {
      clearTimeout(timeout);
      if (code !== 0) reject(new Error((errors || output || ('exit ' + code)).slice(-900)));
      else resolve(output);
    });
    child.stdin.on('error', () => {});
    child.stdin.end(input ?? undefined);
  });
}
try {
  if (!['check', 'deploy'].includes(mode)) throw new Error('Unsupported action');
  const bytes = [];
  let total = 0;
  for await (const chunk of process.stdin) {
    total += chunk.length;
    if (total > 4096) throw new Error('Config too large');
    bytes.push(chunk);
  }
  const config = JSON.parse(Buffer.concat(bytes).toString('utf8'));
  const { domain, host, user, port, desktopId, keyPath } = config;
  if (!checkHost(host) || !checkHost(domain) ||
      !/^[a-z_][a-z0-9_-]{0,31}$/.test(user) ||
      !Number.isInteger(port) || port < 1 || port > 65535 ||
      !/^d_[a-fA-F0-9]{16,78}$/.test(desktopId)) throw new Error('Invalid deploy parameters');
  if (keyPath && (!path.isAbsolute(keyPath) || !fs.existsSync(keyPath)))
    throw new Error('SSH private key path must be an existing absolute path');
  const installer = path.join(root, 'relay-deploy-install.sh');
  if (!fs.existsSync(installer)) throw new Error('Server installer resource missing');
  if (mode === 'deploy' && (!archivePath || !fs.existsSync(archivePath)))
    throw new Error('Relay source bundle missing');
  const ssh = locate('ssh');
  const scp = locate('scp');
  const opts = ['-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
    '-o', 'ConnectTimeout=8', '-o', 'ServerAliveInterval=15',
    '-o', 'ServerAliveCountMax=3'];
  if (keyPath) opts.push('-i', keyPath);
  const remote = user + '@' + host;
  const elevation = user === 'root' ? 'bash' : 'sudo -n bash';
  function sshCommand(action, remoteArchive = '') {
    return [...opts, '-p', String(port), '-T', remote,
      elevation + ' -s -- ' + action + ' ' + domain + ' ' + desktopId +
      (remoteArchive ? ' ' + remoteArchive : '')];
  }
  emit('检查 SSH、Nginx、HTTPS 站点及 Rust 编译环境');
  const script = fs.readFileSync(installer);
  const preflight = await exec(ssh, sshCommand('check'), script, true, 35);
  if (mode === 'check') {
    result({ ok: true, message: preflight.trim().slice(-700) });
  } else {
    const temp = '/tmp/chatx-relay-' + crypto.randomBytes(12).toString('hex') + '.tar.gz';
    emit('SSH 预检成功，上传服务器源码包');
    await exec(scp, [...opts, '-P', String(port),
      archivePath, remote + ':' + temp], undefined, false, 90);
    try {
      emit('上传完成，服务器执行编译与自动回滚保护');
      const remoteResult = await exec(ssh, sshCommand('deploy', temp),
        script, true, 1200);
      const message = remoteResult.split(/\r?\n/).find(line => line.startsWith('DEPLOY_OK '));
      if (!message) throw new Error('No deployment confirmation');
      const bootstrapLine = remoteResult.split(/\r?\n/)
        .find(line => /^CHATX_NEW_BOOTSTRAP_TOKEN=[a-f0-9]{64}$/.test(line));
      result({
        ok: true, message,
        ...(bootstrapLine ? { bootstrapToken: bootstrapLine.split('=')[1] } : {}),
      });
    } finally {
      try {
        await exec(ssh, [...opts, '-p', String(port), '-T', remote,
          elevation + " -c 'rm -f " + temp + "'"], undefined, false, 15);
      } catch { /* Staged archive cleanup must not hide the original outcome. */ }
    }
  }
} catch (error) {
  result({ ok: false, message: safeError(error) });
  process.exitCode = 1;
}
