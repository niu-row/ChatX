export type InvocationStatus = 'ok' | 'error';

export type InvocationLogEntry = {
  id: number;
  startedAt: string;
  tool: string;
  status: InvocationStatus;
  durationMs: number;
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

export function clearInvocationLogForTests(): void {
  entries.length = 0;
  nextId = 1;
}
