"""Isolated plugin behavior tests; never contact the panel or production Relay."""
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('chatx_plugin', Path(__file__).parent / 'chatx_relay/chatx_relay_main.py')
plugin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plugin)


class PluginTests(unittest.TestCase):
    def test_codes_have_signed_origin_expiry_and_random_nonce(self):
        first = plugin.make_invite('https://relay.example.com/', 'admin-secret', 1000)
        second = plugin.make_invite('https://relay.example.com', 'admin-secret', 1000)
        prefix, body, signature = first['code'].split('.')
        self.assertEqual(prefix, 'cxr1')
        payload = json.loads(bytes.fromhex(body))
        self.assertEqual(payload['baseUrl'], 'https://relay.example.com')
        self.assertEqual(payload['expiresAt'], 601000)
        self.assertEqual(len(payload['nonce']), 32)
        self.assertNotEqual(first['code'], second['code'])
        key = hashlib.sha256(b'admin-secret').hexdigest().encode('ascii')
        self.assertEqual(signature, hmac.new(key, ('chatx-registration-v1:' + body).encode(), hashlib.sha256).hexdigest())
        self.assertNotIn('admin-secret', json.dumps(first))
        for url in ['http://relay.example.com', 'https://a.example/path', 'https://user:pass@example.com',
                    'https://example.com:443', 'https://-example.com', 'https://example..com']:
            with self.assertRaises(ValueError):
                plugin.make_invite(url, 'admin-secret', 1000)

    def test_all_panel_actions_reject_unauthenticated_or_dynamic_routes(self):
        instance = plugin.chatx_relay_main()
        for login, uid, route, method in [(False, 1, '/plugin', 'POST'), (True, 2, '/plugin', 'POST'),
                                        (True, 1, '/chatx_relay/create_invite.json', 'POST'),
                                        (True, 1, '/plugin', 'GET')]:
            flask = SimpleNamespace(has_request_context=lambda: True, session={'login': login, 'uid': uid},
                                    request=SimpleNamespace(path=route, method=method))
            with patch.dict(sys.modules, {'flask': flask}):
                for name in ('get_status', 'create_invite', 'select_desktop', 'restart_relay'):
                    self.assertFalse(getattr(instance, name)(SimpleNamespace()).get('status'))

    def test_status_only_returns_safe_metadata(self):
        instance = plugin.chatx_relay_main()
        with patch.object(instance, '_check', return_value=True), patch.object(plugin.os.path, 'isfile', return_value=True), \
                patch.object(plugin, 'read_env', return_value={'CHATX_MCP_DESKTOP_ID': 'd_test'}), \
                patch.object(instance, '_url', return_value='https://relay.example.com'), \
                patch.object(plugin, 'service_command', return_value=SimpleNamespace(returncode=0)), \
                patch.object(plugin, 'health', return_value={'ok': True, 'onlineDesktops': 1, 'registrationCodesSupported': True}), \
                patch.object(plugin, 'registry', return_value={'desktops': {'d_test': {
                    'tokenHash': 'never-expose-hash', 'deviceName': '<script>alert(1)</script>',
                    'devices': {'phone': {'tokenHash': 'never-expose-phone'}}}}}):
            result = instance.get_status(SimpleNamespace())
        self.assertTrue(result['status'])
        self.assertNotIn('never-expose', json.dumps(result))
        self.assertEqual(result['data']['desktops'][0]['pairedDevices'], 1)
        self.assertTrue(result['data']['desktops'][0]['selected'])

    def test_target_switch_preserves_registry_and_rolls_back_on_restart_failure(self):
        instance = plugin.chatx_relay_main()
        with tempfile.TemporaryDirectory() as directory:
            envfile = os.path.join(directory, 'relay.env')
            registryfile = os.path.join(directory, 'registry.json')
            desktop_id = 'd_' + 'a' * 32
            previous = 'CHATX_RELAY_DATA=' + registryfile + '\nCHATX_RELAY_BOOTSTRAP_TOKEN=admin-secret\nCHATX_MCP_DESKTOP_ID=d_old\n'
            Path(envfile).write_text(previous, encoding='utf-8')
            registry_text = json.dumps({'desktops': {desktop_id: {'devices': {'phone': {'tokenHash': 'phone'}}}}})
            Path(registryfile).write_text(registry_text, encoding='utf-8')
            flock = SimpleNamespace(LOCK_EX=2, flock=lambda *args: None)
            with patch.object(instance, '_check', return_value=True), patch.object(plugin, 'ENV_FILE', envfile), \
                    patch.object(plugin, 'PANEL_LOCK', os.path.join(directory, 'panel.lock')), \
                    patch.dict(sys.modules, {'fcntl': flock}), \
                    patch.object(plugin.os, 'fchmod', create=True), patch.object(plugin.os, 'fchown', create=True), \
                    patch.object(plugin, 'service_command', return_value=SimpleNamespace(returncode=1)):
                result = instance.select_desktop(SimpleNamespace(desktop_id=desktop_id))
            self.assertFalse(result['status'])
            self.assertEqual(Path(envfile).read_text(encoding='utf-8'), previous)
            self.assertEqual(Path(registryfile).read_text(encoding='utf-8'), registry_text)


if __name__ == '__main__':
    unittest.main()
