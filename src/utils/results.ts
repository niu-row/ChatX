import { config } from '../config.js';

export function textResult(value: unknown) {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  const content = [{ type: 'text' as const, text }];
  const result = value && typeof value === 'object' && !Array.isArray(value)
    ? { content, structuredContent: value as Record<string, unknown> }
    : { content };
  const bytes = Buffer.byteLength(JSON.stringify(result));
  if (bytes > config.maxMcpResponseBytes) {
    throw new Error(
      `MCP response would be ${bytes} bytes; limit is ${config.maxMcpResponseBytes}. Narrow the request or use pagination.`,
    );
  }
  return result;
}

export function resultPayloadBytes(result: unknown): number | null {
  try {
    return Buffer.byteLength(JSON.stringify(result));
  } catch {
    return null;
  }
}

export function errorResult(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  return {
    isError: true,
    content: [{ type: 'text' as const, text: message }],
  };
}

export function truncateText(value: string, maxChars: number): { text: string; truncated: boolean } {
  if (value.length <= maxChars) return { text: value, truncated: false };
  return {
    text: value.slice(0, maxChars) + `\n\n[truncated ${value.length - maxChars} characters]`,
    truncated: true,
  };
}
