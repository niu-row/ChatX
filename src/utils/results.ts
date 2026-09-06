export function textResult(value: unknown) {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  return { content: [{ type: 'text' as const, text }] };
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
