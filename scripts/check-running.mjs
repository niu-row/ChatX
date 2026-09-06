const port = Number.parseInt(process.argv[2] || '3210', 10);
if (!Number.isInteger(port) || port < 1 || port > 65535) process.exit(1);
try {
  const response = await fetch(`http://127.0.0.1:${port}/healthz`, { signal: AbortSignal.timeout(1200) });
  if (!response.ok) process.exit(1);
  const body = await response.json();
  process.exit(body?.ok === true && body?.service === 'chatx' ? 0 : 1);
} catch {
  process.exit(1);
}
