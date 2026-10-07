(() => {
  window.ChatXPages = window.ChatXPages || {};

  window.ChatXPages.createCallHistoryPage = ({
    $,
    invoke,
    normalizeError,
  }) => {
    let busy = false;

    function jsonText(value) {
      try {
        return JSON.stringify(value ?? null, null, 2);
      } catch {
        return String(value);
      }
    }

    function timeLabel(value) {
      const date = new Date(value);
      return Number.isNaN(date.getTime())
        ? String(value || '—')
        : date.toLocaleString();
    }
    function appendDetail(host, title, value) {
      const section = document.createElement('div');
      section.className = 'call-detail-section';
      const heading = document.createElement('h4');
      heading.textContent = title;
      const block = document.createElement('pre');
      block.textContent = jsonText(value);
      section.append(heading, block);
      host.append(section);
    }

    function render(payload) {
      const items = Array.isArray(payload?.items)
        ? payload.items
        : [];
      const toolNames = Array.isArray(payload?.toolNames)
        ? payload.toolNames
        : [];
      const toolSelect = $('callToolFilter');
      const selectedTool = toolSelect.value;
      toolSelect.textContent = '';
      const allTools = document.createElement('option');
      allTools.value = '';
      allTools.textContent = '全部工具';
      toolSelect.append(allTools);
      for (const toolName of toolNames) {
        const option = document.createElement('option');
        option.value = toolName;
        option.textContent = toolName;
        toolSelect.append(option);
      }
      toolSelect.value = toolNames.includes(selectedTool)
        ? selectedTool
        : '';

      const filteredTotal = Number(
        payload?.filteredTotal ?? items.length,
      );
      const total = Number(payload?.total ?? filteredTotal);
      const invalid = Number(payload?.invalidEntries || 0);
      $('callHistorySummary').textContent =
        `${filteredTotal} / ${total} 条，当前显示 ${items.length} 条${invalid ? `；忽略 ${invalid} 条无效记录` : ''}`;

      const stats = payload?.stats || {};
      const successCount = Number(stats.successCount || 0);
      const averageDuration = stats.averageDurationMs == null
        ? Number.NaN
        : Number(stats.averageDurationMs);
      const p95Duration = stats.p95DurationMs == null
        ? Number.NaN
        : Number(stats.p95DurationMs);
      $('callStatCount').textContent = String(filteredTotal);
      $('callStatSuccess').textContent = filteredTotal
        ? `${Math.round(successCount / filteredTotal * 1000) / 10}%`
        : '—';
      $('callStatAvg').textContent =
        Number.isFinite(averageDuration)
          ? `${averageDuration} ms`
          : '—';
      $('callStatP95').textContent =
        Number.isFinite(p95Duration)
          ? `${p95Duration} ms`
          : '—';

      const host = $('callHistory');
      host.textContent = '';
      if (!items.length) {
        const empty = document.createElement('p');
        empty.className = 'muted';
        empty.textContent =
          '没有符合当前筛选条件的调用记录。';
        host.append(empty);
        return;
      }
      for (const item of items) {
        const details = document.createElement('details');
        details.className = 'call-record';
        const summary = document.createElement('summary');
        summary.className = 'call-record-summary';

        const time = document.createElement('span');
        time.className = 'call-time';
        time.textContent = timeLabel(item.timestamp);
        const tool = document.createElement('code');
        tool.textContent = String(item.toolName || 'unknown');
        const status = document.createElement('span');
        status.className =
          `call-status ${item.success === false ? 'bad' : 'ok'}`;
        status.textContent =
          item.success === false ? '失败' : '成功';
        const duration = document.createElement('span');
        duration.className = 'call-duration';
        duration.textContent = Number.isFinite(Number(item.duration))
          ? `${Number(item.duration)} ms`
          : '—';
        summary.append(time, tool, status, duration);
        const detail = document.createElement('div');
        detail.className = 'call-record-detail';
        appendDetail(detail, '参数', item.arguments ?? {});
        appendDetail(detail, '返回结果', item.output ?? null);
        details.append(summary, detail);
        host.append(details);
      }
    }

    async function refresh() {
      if (!invoke || busy) return;
      busy = true;
      $('refreshCalls').disabled = true;
      $('clearCalls').disabled = true;
      try {
        const limit = Number.parseInt(
          $('callLimit').value,
          10,
        );
        const args = {
          limit: Number.isFinite(limit) ? limit : 200,
        };
        const toolName = $('callToolFilter').value;
        const status = $('callStatusFilter').value;
        if (toolName) args.toolName = toolName;
        if (status) args.status = status;
        render(await invoke('get_call_history', args));
      } catch (error) {
        $('callHistorySummary').textContent = '读取失败';
        $('callHistory').textContent = normalizeError(error);
      } finally {
        busy = false;
        $('refreshCalls').disabled = false;
        $('clearCalls').disabled = false;
      }
    }

    async function clear() {
      if (!invoke || busy) return;
      if (
        typeof window.confirm === 'function'
        && !window.confirm(
          '确认清空本机调用记录？此操作无法撤销。',
        )
      ) {
        return;
      }

      busy = true;
      $('refreshCalls').disabled = true;
      $('clearCalls').disabled = true;
      let cleared = false;
      try {
        await invoke('clear_call_history');
        cleared = true;
      } catch (error) {
        $('callHistorySummary').textContent = '清空失败';
        $('callHistory').textContent = normalizeError(error);
      } finally {
        busy = false;
        $('refreshCalls').disabled = false;
        $('clearCalls').disabled = false;
      }
      if (cleared) await refresh();
    }

    return Object.freeze({
      refresh,
      clear,
      render,
    });
  };
})();
