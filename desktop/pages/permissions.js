(() => {
  window.ChatXPages = window.ChatXPages || {};

  window.ChatXPages.createPermissionsPage = ({
    $,
    invoke,
    normalizeError,
    showError,
    beginRefresh,
    isLatestRefresh,
  }) => {
    let busy = false;

    function statusLabel(status) {
      return {
        granted: '已授权',
        denied: '未授权',
        pending: '待授权',
        notNeeded: '无需授权',
        unavailable: '不可用',
        error: '检查失败',
      }[status] || String(status || '未知');
    }

    function render(payload) {
      const supported = Boolean(payload?.supported);
      const host = $('permissionList');
      host.textContent = '';
      if (!supported) {
        const note = document.createElement('p');
        note.className = 'muted';
        note.textContent =
          '当前平台不需要 macOS TCC 文件夹预授权。';
        host.append(note);
      } else {
        for (const item of payload?.items || []) {
          const row = document.createElement('div');
          row.className = 'permission-row';
          const copy = document.createElement('div');
          const title = document.createElement('strong');
          title.textContent = String(
            item.label || item.id || '权限',
          );
          const detail = document.createElement('small');
          detail.textContent = String(item.detail || '');
          copy.append(title, detail);

          const badge = document.createElement('span');
          const status = String(item.status || 'pending');
          badge.className =
            `setting-status ${status === 'granted' || status === 'notNeeded' ? 'ok' : status === 'denied' || status === 'error' ? 'bad' : status === 'pending' ? 'warn' : 'off'}`;
          badge.textContent = statusLabel(status);
          row.append(copy, badge);
          host.append(row);
        }
      }

      $('authorizeAllPermissions').disabled =
        busy || !supported;
      $('openFullDiskAccess').disabled =
        busy || !supported;
      $('fullDiskAccessHint').textContent = String(
        payload?.fullDiskAccess?.detail
        || 'macOS 不允许应用静默授予“完全磁盘访问”。',
      );
    }

    async function refresh() {
      if (!invoke || busy) return;
      const version = beginRefresh('permissions');
      try {
        const payload = await invoke(
          'get_permission_center',
        );
        if (isLatestRefresh('permissions', version)) {
          render(payload);
        }
      } catch (error) {
        if (isLatestRefresh('permissions', version)) {
          $('permissionList').textContent =
            normalizeError(error);
        }
      }
    }

    async function requestAll() {
      if (!invoke || busy) return;
      busy = true;
      $('authorizeAllPermissions').disabled = true;
      $('authorizeAllPermissions').textContent =
        '正在集中授权…';
      showError('');
      try {
        render(await invoke('request_all_permissions'));
      } catch (error) {
        showError(normalizeError(error));
      } finally {
        busy = false;
        $('authorizeAllPermissions').textContent =
          '一次触发全部授权';
        await refresh();
      }
    }

    async function openFullDiskAccess() {
      try {
        await invoke('open_full_disk_access_settings');
      } catch (error) {
        showError(normalizeError(error));
      }
    }

    return Object.freeze({
      render,
      refresh,
      requestAll,
      openFullDiskAccess,
    });
  };
})();
