export type InvocationStatus = 'ok' | 'error';

export type InvocationLogEntry = {
  id: number;
  startedAt: string;
  tool: string;
  status: InvocationStatus;
  durationMs: number;
  resultBytes: number | null;
};

const MAX_ENTRIES = 500;
const entries: InvocationLogEntry[] = [];
let nextId = 1;

export function recordInvocation(entry: Omit<InvocationLogEntry, 'id'>): InvocationLogEntry {
  const stored: InvocationLogEntry = { id: nextId++, ...entry };
  entries.push(stored);
  if (entries.length > MAX_ENTRIES) entries.splice(0, entries.length - MAX_ENTRIES);
  return stored;
}

export function getInvocationLog(limit = 50): InvocationLogEntry[] {
  const safeLimit = Math.max(1, Math.min(MAX_ENTRIES, Math.trunc(limit) || 50));
  return entries.slice(-safeLimit).reverse().map((entry) => ({ ...entry }));
}

export function getInvocationSummary(): Array<Record<string, unknown>> {
  const groups = new Map<string, InvocationLogEntry[]>();
  for (const entry of entries) {
    const group = groups.get(entry.tool);
    if (group) group.push(entry);
    else groups.set(entry.tool, [entry]);
  }

  return [...groups.entries()]
    .map(([tool, group]) => {
      const durations = group.map((entry) => entry.durationMs).sort((a, b) => a - b);
      const resultBytes = group
        .map((entry) => entry.resultBytes)
        .filter((value): value is number => typeof value === 'number');
      const percentile = (ratio: number) =>
        durations[Math.min(durations.length - 1, Math.floor(durations.length * ratio))] ?? 0;
      return {
        tool,
        count: group.length,
        errors: group.filter((entry) => entry.status === 'error').length,
        avg_ms: Math.round((durations.reduce((total, value) => total + value, 0) / durations.length) * 10) / 10,
        p50_ms: percentile(0.5),
        p95_ms: percentile(0.95),
        max_ms: durations.at(-1) ?? 0,
        avg_result_bytes:
          resultBytes.length > 0
            ? Math.round(resultBytes.reduce((total, value) => total + value, 0) / resultBytes.length)
            : null,
      };
    })
    .sort((a, b) => Number(b.p95_ms) - Number(a.p95_ms));
}

export function clearInvocationLogForTests(): void {
  entries.length = 0;
  nextId = 1;
}
