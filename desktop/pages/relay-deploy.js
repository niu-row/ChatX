(() => {
  window.ChatXPages = window.ChatXPages || {};
  window.ChatXPages.createRelayDeployController = ({ $, invoke, normalizeError }) => {
    let busy = false;
    let validated = '';
    let dirty = false;
    let lines = [];
    const ids = ['relayDeployHost', 'relayDeployUser', 'relayDeployPort',
      'relayDeployDomain', 'relayDeployDesktopId', 'relayDeployKeyPath'];

    function log(message, reset = false) {
      if (reset) lines = [];
      lines.push(String(message));
      lines = lines.slice(-24);
      $('relayDeployLog').textContent = lines.join('\n');
    }
    function config() {
      return {
        host: $('relayDeployHost').value.trim(),
        user: $('relayDeployUser').value.trim(),
        port: Number($('relayDeployPort').value),
        domain: $('relayDeployDomain').value.trim().toLowerCase(),
        desktopId: $('relayDeployDesktopId').value.trim(),
        keyPath: $('relayDeployKeyPath').value.trim(),
      };
    }
    function invalidate() {
      dirty = true;
      validated = '';
      $('relayDeployApply').disabled = true;
    }
    function autofill() {
      if (dirty) return;
      if (!$('relayDeployHost').value) $('relayDeployHost').value = $('httpsMcpHost').value || '';
      if (!$('relayDeployDomain').value) {
        $('relayDeployDomain').value =
          $('httpsMcpUrl').value.trim().replace(/^https:\/\//, '').split('/')[0] || '';
      }
    }
    async function action(apply) {
      if (!invoke || busy) return;
      const target = config();
      const fingerprint = JSON.stringify(target);
      if (apply && validated !== fingerprint) {
        log('服务器信息变更后必须重新预检。');
        return;
      }
      if (apply && !window.confirm(
        '即将通过 SSH 更新服务器 Relay 和 Nginx，期间服务会短暂重启。已有配置会先备份，失败自动回滚。继续吗？'
      )) return;
      busy = true;
      $('relayDeployCheck').disabled = true;
      $('relayDeployApply').disabled = true;
      log(apply ? '开始部署（远端 Cargo 编译可能需要数分钟）' : '开始只读预检…', true);
      try {
        const response = await invoke('manage_relay_server', { config: target, apply });
        if (!response?.ok) throw new Error(response?.message || '服务器未返回成功状态');
        if (!apply) validated = fingerprint;
        log(response.message || '成功');
      } catch (error) {
        validated = '';
        log('失败：' + normalizeError(error));
      } finally {
        busy = false;
        $('relayDeployCheck').disabled = false;
        $('relayDeployApply').disabled = validated !== JSON.stringify(config());
      }
    }
    function bind() {
      for (const id of ids) $(id).addEventListener('input', invalidate);
      if (invoke) {
        // Can create a stable local identity before registering a new server.
        invoke('prepare_relay_deploy_desktop_id').then((desktopId) => {
          if (!dirty && /^d_[a-fA-F0-9]{16,78}$/.test(desktopId || '')) {
            $('relayDeployDesktopId').value = desktopId;
          }
        }).catch((error) => {
          log('无法读取本机 Desktop ID：' + normalizeError(error));
        });
      }
      $('relayDeployCheck').addEventListener('click', () => action(false));
      $('relayDeployApply').addEventListener('click', () => action(true));
      if (window.__TAURI__?.event?.listen) {
        window.__TAURI__.event.listen('relay-deploy-progress', (event) => {
          if (busy && event?.payload?.message) log(event.payload.message);
        }).catch(() => {});
      }
    }
    return { bind, autofill };
  };
})();
