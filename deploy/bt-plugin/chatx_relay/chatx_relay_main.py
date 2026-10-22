# coding: utf-8
"""Authenticated Baota panel management. No public administration routes."""
import hashlib
import hmac
import json
import os
import re
import secrets
import shutil
import stat
import subprocess
import tempfile
import time
import urllib.request

ENV_FILE = '/etc/chatx-relay/relay.env'
PANEL_CONFIG = '/etc/chatx-relay/panel.json'
PANEL_LOCK = '/etc/chatx-relay/panel.lock'
SERVICE = 'chatx-relay.service'


def read_env():
    with open(ENV_FILE, encoding='utf-8') as stream:
        return dict(line.strip().split('=', 1) for line in stream
                    if line.strip() and not line.lstrip().startswith('#') and '=' in line)


def public_url(value):
    value = str(value).strip().rstrip('/')
    host = value[8:] if value.startswith('https://') else ''
    if not host or len(host) > 253 or any(
        not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?', label)
        for label in host.split('.')
    ):
        raise ValueError('请填写不带端口和路径的 HTTPS 域名，例如 https://relay.example.com')
    return value


def make_invite(url, bootstrap, now_ms=None):
    if not bootstrap.strip():
        raise ValueError('Relay 未配置注册密钥，请先完成服务器部署。')
    expires = (int(time.time() * 1000) if now_ms is None else now_ms) + 600000
    payload = {'baseUrl': public_url(url), 'expiresAt': expires, 'nonce': secrets.token_hex(16)}
    body = json.dumps(payload, separators=(',', ':')).encode('utf-8').hex()
    key = hashlib.sha256(bootstrap.strip().encode('utf-8')).hexdigest().encode('ascii')
    signature = hmac.new(key, ('chatx-registration-v1:' + body).encode('ascii'), hashlib.sha256).hexdigest()
    return {'code': 'cxr1.' + body + '.' + signature, 'expiresAt': expires}


def atomic_write(path, data, preserve=False):
    directory = os.path.dirname(path)
    metadata = os.stat(path) if preserve else None
    descriptor, temporary = tempfile.mkstemp(prefix='.chatx-panel-', dir=directory)
    try:
        with os.fdopen(descriptor, 'w', encoding='utf-8') as stream:
            os.fchmod(stream.fileno(), stat.S_IMODE(metadata.st_mode) if metadata else 0o600)
            if metadata:
                os.fchown(stream.fileno(), metadata.st_uid, metadata.st_gid)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def service_command(action):
    # Arguments are fixed by the caller; never execute request strings in a shell.
    return subprocess.run(['systemctl', action, SERVICE], stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, universal_newlines=True, timeout=12)


def registry(env):
    path = env.get('CHATX_RELAY_DATA', '/var/lib/chatx-relay/registry.json')
    if not os.path.exists(path):
        return {'desktops': {}}
    with open(path, encoding='utf-8') as stream:
        return json.load(stream)


def health(env):
    bind = env.get('CHATX_RELAY_BIND', '127.0.0.1:8787')
    match = re.fullmatch(r'(127\.0\.0\.1|\[::1\]):([0-9]{1,5})', bind)
    if not match or not 0 < int(match.group(2)) <= 65535:
        raise ValueError('管理插件只支持 Relay 监听回环地址，请检查 CHATX_RELAY_BIND。')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open('http://' + bind + '/healthz', timeout=3) as response:
        return json.loads(response.read(65536).decode('utf-8'))


def wait_for_health(env):
    for _ in range(10):
        try:
            if health(env).get('ok') is True:
                return True
        except (OSError, ValueError):
            pass
        time.sleep(0.2)
    return False


class chatx_relay_main:
    def _check(self):
        # Baota's dynamic plugin URLs do not inherit panel authentication.
        # Every entry point also checks this guard, including read-only status.
        from flask import has_request_context, request, session
        return (has_request_context() and bool(session.get('login'))
                and session.get('uid', 1) in (1, '1')
                and request.path.rstrip('/') == '/plugin' and request.method == 'POST')

    def _denied(self):
        return {'status': False, 'msg': '请从已登录的宝塔管理员面板打开 ChatX Relay 插件。'}

    def _url(self):
        if not os.path.isfile(PANEL_CONFIG):
            return ''
        with open(PANEL_CONFIG, encoding='utf-8') as stream:
            return public_url(json.load(stream).get('baseUrl', ''))

    def get_status(self, args):
        if not self._check():
            return self._denied()
        if not os.path.isfile(ENV_FILE):
            return {'status': True, 'data': {'installed': False, 'publicUrl': '', 'desktops': []}}
        try:
            env = read_env()
            selected = env.get('CHATX_MCP_DESKTOP_ID', '')
            desktops = []
            for desktop_id, record in registry(env).get('desktops', {}).items():
                desktops.append({'desktopId': desktop_id, 'deviceName': record.get('deviceName', ''),
                                 'platform': record.get('platform', ''), 'appVersion': record.get('appVersion', ''),
                                 'pairedDevices': len(record.get('devices', {})), 'selected': desktop_id == selected})
            probe, error = {}, ''
            try:
                probe = health(env)
            except (OSError, ValueError):
                error = 'Relay 健康检查未通过，请启动服务并检查服务器部署。'
            gateway = probe.get('mcpGateway', {})
            return {'status': True, 'data': {
                'installed': True, 'active': service_command('is-active').returncode == 0,
                'publicUrl': self._url(), 'healthy': probe.get('ok') is True, 'healthError': error,
                'registrationCodesSupported': probe.get('registrationCodesSupported') is True,
                'monitorDesktops': probe.get('onlineDesktops', 0), 'mcpConnected': gateway.get('connected') is True,
                'mcpProtocol': gateway.get('transportProtocol'), 'mcpDesktopId': selected,
                'desktops': sorted(desktops, key=lambda d: d['deviceName']),
            }}
        except (OSError, ValueError, subprocess.SubprocessError):
            return {'status': False, 'msg': '无法读取 Relay 配置，请检查服务部署与文件权限。'}

    def create_invite(self, args):
        if not self._check():
            return self._denied()
        try:
            env = read_env()
            if health(env).get('registrationCodesSupported') is not True:
                return {'status': False, 'msg': '服务器版本尚不支持一次性注册码，请先更新 Relay 服务。'}
            url = public_url(getattr(args, 'public_url', '') or self._url())
            result = make_invite(url, env.get('CHATX_RELAY_BOOTSTRAP_TOKEN', ''))
            atomic_write(PANEL_CONFIG, json.dumps({'baseUrl': url}, ensure_ascii=False))
            return {'status': True, 'data': result}
        except ValueError as error:
            return {'status': False, 'msg': str(error)}
        except OSError:
            return {'status': False, 'msg': '无法读取注册配置或连接 Relay 服务。'}

    def select_desktop(self, args):
        if not self._check():
            return self._denied()
        desktop_id = str(getattr(args, 'desktop_id', ''))
        if not re.fullmatch(r'd_[a-fA-F0-9]{16,78}', desktop_id):
            return {'status': False, 'msg': '电脑标识无效，请刷新电脑列表后重试。'}
        try:
            import fcntl
            with open(PANEL_LOCK, 'a') as lock:
                os.fchmod(lock.fileno(), 0o600)
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
                env = read_env()
                if desktop_id not in registry(env).get('desktops', {}):
                    return {'status': False, 'msg': '这台电脑尚未注册，请先在 ChatX 粘贴注册码。'}
                if env.get('CHATX_MCP_DESKTOP_ID') == desktop_id:
                    return {'status': True, 'msg': '这台电脑已经是 ChatGPT MCP 主机。'}
                with open(ENV_FILE, encoding='utf-8') as stream:
                    previous = stream.read()
                backup = ENV_FILE + '.panel-backup-' + time.strftime('%Y%m%d-%H%M%S') + '-' + secrets.token_hex(4)
                shutil.copy2(ENV_FILE, backup)
                lines = [line for line in previous.splitlines() if not line.startswith('CHATX_MCP_DESKTOP_ID=')]
                lines.append('CHATX_MCP_DESKTOP_ID=' + desktop_id)
                atomic_write(ENV_FILE, '\n'.join(lines) + '\n', preserve=True)
                try:
                    if service_command('restart').returncode != 0:
                        raise OSError('restart failed')
                    # Restart succeeding only proves systemd accepted the job.
                    if not wait_for_health(read_env()):
                        raise OSError('health check failed')
                except (OSError, subprocess.SubprocessError):
                    atomic_write(ENV_FILE, previous, preserve=True)
                    service_command('restart')
                    return {'status': False, 'msg': '切换后服务未就绪，已恢复原来的 MCP 主机配置。'}
                return {'status': True, 'msg': 'MCP 主机已切换，请在这台电脑的 ChatX 启动 HTTPS MCP。'}
        except (OSError, ValueError, subprocess.SubprocessError):
            return {'status': False, 'msg': '切换失败，请检查 Relay 服务；现有电脑注册记录保留。'}

    def restart_relay(self, args):
        if not self._check():
            return self._denied()
        try:
            if service_command('restart').returncode == 0:
                return {'status': True, 'msg': '已请求启动 Relay，请刷新状态确认健康检查与桌面连接。'}
        except (OSError, subprocess.SubprocessError):
            pass
        return {'status': False, 'msg': 'Relay 启动失败，请检查 systemd 中的 chatx-relay.service。'}
