(() => {
  window.ChatXPages = window.ChatXPages || {};
  window.ChatXPages.createOauthSecurityPage = ({
    $, invoke, showError, normalizeError, getBusy, setBusy, refreshConnection,
  }) => {
    let policyDirty = false;
    let committedHours = 168;
    let grantRows = [];
    const lifetimeOptions = [1, 24, 168, 720];

    function chosenHours() {
      const choice = $('httpsMcpAuthorizationDuration').value;
      if (choice === 'unlimited') return null;
      const hours = choice === 'custom'
        ? Number($('httpsMcpCustomHours').value) : Number(choice);
      if (!Number.isInteger(hours) || hours < 1 || hours > 87600) {
        throw new Error('授权有效期必须为 1～87600 小时，或选择「不限时间」');
      }
      return hours;
    }
    function updateLifetimeVisibility() {
      $('httpsMcpCustomHoursWrap').hidden =
        $('httpsMcpAuthorizationDuration').value !== 'custom';
      $('httpsMcpUnlimitedWarning').hidden =
        $('httpsMcpAuthorizationDuration').value !== 'unlimited';
    }
    function renderPolicy(s) {
      const ttl = s?.authorizationTtlHours === undefined ? 168 : s.authorizationTtlHours;
      committedHours = ttl;
      if (!policyDirty) {
        $('httpsMcpAuthorizationDuration').value = ttl === null ? 'unlimited'
          : lifetimeOptions.includes(ttl) ? String(ttl) : 'custom';
        if (ttl !== null) $('httpsMcpCustomHours').value = ttl;
      }
      updateLifetimeVisibility();
      $('saveHttpsMcpAuthorizationDuration').disabled = getBusy();
      $('rotateHttpsPassword').disabled = getBusy();
      $('revokeHttpsMcpAuthorization').disabled = getBusy();
      $('revokeAllHttpsMcpAuthorizations').disabled = getBusy();
    }
    function grantLabel(grant) {
      const expired = grant.expiresAt !== null &&
        Number(grant.expiresAt) <= Date.now() / 1000;
      const state = grant.revoked ? '已撤销' : expired ? '已过期' : '有效';
      const expiration = grant.expiresAt === null ? '不限时间'
        : new Date(Number(grant.expiresAt) * 1000).toLocaleString('zh-CN');
      const source = grant.id === 'legacy' ? '旧版迁移授权'
        : `ChatGPT · ${String(grant.clientTag || '').slice(0, 12)}`;
      return `${source} · ${state} · 到期：${expiration}`;
    }
    function selectedGrantActive() {
      const grant = grantRows.find((row) =>
        row.id === $('httpsMcpAuthorizationList').value);
      return Boolean(grant && !grant.revoked &&
        (grant.expiresAt === null || Number(grant.expiresAt) > Date.now() / 1000));
    }
    function refreshSelectedGrantAction() {
      $('revokeHttpsMcpAuthorization').disabled = getBusy() || !selectedGrantActive();
    }
    function renderGrants(response) {
      grantRows = Array.isArray(response?.grants) ? response.grants : [];
      const list = $('httpsMcpAuthorizationList');
      const previous = list.value;
      list.innerHTML = '';
      if (typeof document.createElement === 'function') {
        for (const grant of grantRows) {
          const option = document.createElement('option');
          option.value = grant.id;
          option.textContent = grantLabel(grant);
          if (grant.id === previous) option.selected = true;
          list.appendChild(option);
        }
      }
      const active = grantRows.filter((g) => !g.revoked &&
        (g.expiresAt === null || Number(g.expiresAt) > Date.now() / 1000)).length;
      $('httpsMcpAuthorizationSummary').textContent =
        `共 ${grantRows.length} 条授权记录，其中 ${active} 条有效。改变默认期限不影响现有授权。`;
      refreshSelectedGrantAction();
      $('revokeAllHttpsMcpAuthorizations').disabled = getBusy() || !active;
    }
    async function refreshGrants() {
      try { renderGrants(await invoke('get_https_mcp_authorizations')); }
      catch (error) {
        $('httpsMcpAuthorizationSummary').textContent =
          `读取授权记录失败：${normalizeError(error)}`;
      }
    }
    async function refreshSecurity() {
      if (!invoke || getBusy()) return;
      await refreshConnection();
      if (!getBusy()) await refreshGrants();
    }
    async function saveLifetime() {
      if (getBusy()) return;
      let hours;
      try { hours = chosenHours(); }
      catch (error) { showError(normalizeError(error)); return; }
      if (hours === null && !window.confirm(
        '不限时间的授权不会自动失效；已授权的 ChatGPT 可以长期操作本机文件和执行命令，直到你主动撤销。确定保存吗？'
      )) return;
      setBusy(true);
      showError('');
      try {
        await invoke('set_https_mcp_authorization_lifetime', { ttlHours: hours });
        committedHours = hours;
        policyDirty = false;
      } catch (error) { showError(normalizeError(error)); }
      finally { setBusy(false); await refreshConnection(); }
    }
    async function revokeGrant(all) {
      if (getBusy()) return;
      const id = $('httpsMcpAuthorizationList').value;
      if (!all && !selectedGrantActive()) return;
      if (!window.confirm(all
        ? '确定撤销全部 OAuth 授权？ChatGPT 将需要重新授权。'
        : '确定撤销这项 OAuth 授权？现有访问令牌和刷新令牌将失效。'
      )) return;
      setBusy(true);
      showError('');
      try {
        const command = all ? 'revoke_all_https_mcp_authorizations'
          : 'revoke_https_mcp_authorization';
        // Mutation success and refreshing the listing are separate. A damaged
        // unrelated grant must not make a successful revoke look unsuccessful.
        await invoke(command, all ? {} : { grantId: id });
      } catch (error) { showError(normalizeError(error)); }
      finally { setBusy(false); await refreshSecurity(); }
    }
    function hidePassword() {
      const input = $('httpsMcpPassword');
      input.value = '';
      input.type = 'password';
      $('revealHttpsPassword').textContent = '查看 OAuth 授权密码';
    }
    async function rotatePassword() {
      if (getBusy() || !window.confirm(
        '重新生成 OAuth 密码会立即撤销全部 ChatGPT 授权，并重启 HTTPS MCP。需要使用新密码重新授权。确定继续吗？'
      )) return;
      setBusy(true);
      showError('');
      try {
        const next = await invoke('rotate_https_mcp_login_password');
        $('httpsMcpPassword').value = next;
        $('httpsMcpPassword').type = 'text';
        $('revealHttpsPassword').textContent = '隐藏授权密码';
      } catch (error) { showError(normalizeError(error)); }
      finally { setBusy(false); await refreshSecurity(); }
    }
    async function revealPassword() {
      if (!invoke) return;
      const input = $('httpsMcpPassword');
      if (input.type === 'text') {
        input.type = 'password';
        $('revealHttpsPassword').textContent = '查看 OAuth 授权密码';
        return;
      }
      try {
        input.value = await invoke('get_https_mcp_login_password');
        input.type = 'text';
        $('revealHttpsPassword').textContent = '隐藏授权密码';
        input.select();
      } catch (error) { showError(normalizeError(error)); }
    }
    function bind() {
      $('revealHttpsPassword').addEventListener('click', revealPassword);
      $('rotateHttpsPassword').addEventListener('click', rotatePassword);
      $('httpsMcpAuthorizationDuration').addEventListener('change', () => {
        policyDirty = true;
        updateLifetimeVisibility();
      });
      $('httpsMcpCustomHours').addEventListener('input', () => { policyDirty = true; });
      $('saveHttpsMcpAuthorizationDuration').addEventListener('click', saveLifetime);
      $('httpsMcpAuthorizationList').addEventListener('change', refreshSelectedGrantAction);
      $('revokeHttpsMcpAuthorization').addEventListener('click', () => revokeGrant(false));
      $('revokeAllHttpsMcpAuthorizations').addEventListener('click', () => revokeGrant(true));
    }
    return { bind, renderPolicy, refreshSecurity, hidePassword,
      committedHours: () => committedHours };
  };
})();
