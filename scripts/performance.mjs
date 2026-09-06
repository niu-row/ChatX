import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { performance } from 'node:perf_hooks';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';

const port = 43000 + Math.floor(Math.random() * 10000);
const baseUrl = `http://127.0.0.1:${port}`;
const tempRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'chatx-performance-'));
const settingsDir = path.join(tempRoot, '.settings');
let serverLogs = '';
let client;

function percentile(values, ratio) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * ratio))] ?? 0;
}

async function measure(name, operation, iterations = 5) {
  const samples = [];
  let last;
  for (let index = 0; index < iterations; index += 1) {
    const started = performance.now();
    last = await operation();
    samples.push(performance.now() - started);
  }
  return {
    name,
    iterations,
    median_ms: Number(percentile(samples, 0.5).toFixed(2)),
    p95_ms: Number(percentile(samples, 0.95).toFixed(2)),
    min_ms: Number(Math.min(...samples).toFixed(2)),
    last,
  };
}

function jsonOf(result, name) {
  if (result.isError) throw new Error(`${name} failed: ${result.content?.map((item) => item.text ?? '').join('')}`);
  const text = result.content?.find((item) => item.type === 'text')?.text ?? '{}';
  return JSON.parse(text);
}

async function call(name, args) {
  return jsonOf(await client.callTool({ name, arguments: args }), name);
}

const corpus = path.join(tempRoot, 'corpus');
await fs.mkdir(corpus, { recursive: true });
await Promise.all(
  Array.from({ length: 500 }, (_, index) =>
    fs.writeFile(
      path.join(corpus, `file-${String(index).padStart(4, '0')}.txt`),
      `alpha beta gamma ${index}\nneedle-${index % 17}\n`,
      'utf8',
    ),
  ),
);

const server = spawn(process.execPath, ['dist/index.js'], {
  cwd: process.cwd(),
  env: {
    ...process.env,
    CHATGPTX_HOST: '127.0.0.1',
    CHATGPTX_PORT: String(port),
    CHATGPTX_ROOTS: [tempRoot, process.cwd()].join(path.delimiter),
    CHATGPTX_SETTINGS_DIR: settingsDir,
    CHATGPTX_ENABLE_SHELL: 'false',
  },
  stdio: ['ignore', 'pipe', 'pipe'],
});
server.stdout.on('data', (chunk) => { serverLogs += chunk.toString('utf8'); });
server.stderr.on('data', (chunk) => { serverLogs += chunk.toString('utf8'); });

try {
  const startupStarted = performance.now();
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/healthz`);
      if (response.ok) break;
    } catch {
      // Server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  const startupMs = performance.now() - startupStarted;
  if (startupMs >= 15_000) throw new Error(`Server startup timed out.\n${serverLogs}`);

  client = new Client(
    { name: 'chatx-performance', version: '0.1.0' },
    { versionNegotiation: { mode: 'auto' } },
  );
  const connectStarted = performance.now();
  await client.connect(new StreamableHTTPClientTransport(new URL(`${baseUrl}/mcp`)));
  const connectMs = performance.now() - connectStarted;

  const measurements = [
    await measure('tools_list', async () => (await client.listTools()).tools.length),
    await measure('fs_list_metadata_500', async () => {
      const result = await call('fs_list', {
        path: corpus,
        max_entries: 1_000,
        include_metadata: true,
      });
      return result.entry_count;
    }),
    await measure('fs_search_literal', async () => {
      const result = await call('fs_search', { root: corpus, query: 'needle-7', max_results: 500 });
      return { engine: result.search_engine, count: result.result_count };
    }),
    await measure('fs_search_regex', async () => {
      const result = await call('fs_search', { root: corpus, query: 'needle-(7|8)', regex: true, max_results: 500 });
      return { engine: result.search_engine, count: result.result_count };
    }),
    await measure('fs_read_many_50', async () => {
      const result = await call('fs_read_many', {
        files: Array.from({ length: 50 }, (_, index) => ({
          path: path.join(corpus, `file-${String(index).padStart(4, '0')}.txt`),
        })),
        concurrency: 16,
      });
      return result.succeeded;
    }),
  ];

  const limits = {
    startup_ms: 5_000,
    connect_ms: 2_000,
    tools_list_p95_ms: 1_000,
    fs_list_metadata_500_p95_ms: 5_000,
    fs_search_literal_p95_ms: 10_000,
    fs_search_regex_p95_ms: 10_000,
    fs_read_many_50_p95_ms: 5_000,
  };
  const report = {
    generated_at: new Date().toISOString(),
    platform: process.platform,
    node: process.version,
    startup_ms: Number(startupMs.toFixed(2)),
    connect_ms: Number(connectMs.toFixed(2)),
    measurements,
    limits,
  };
  console.log(JSON.stringify(report, null, 2));

  const failures = [];
  if (startupMs > limits.startup_ms) failures.push('startup');
  if (connectMs > limits.connect_ms) failures.push('connect');
  for (const item of measurements) {
    const limit = limits[`${item.name}_p95_ms`];
    if (limit !== undefined && item.p95_ms > limit) failures.push(item.name);
  }
  if (failures.length > 0) throw new Error(`Performance regression threshold exceeded: ${failures.join(', ')}`);
} finally {
  if (client) await client.close().catch(() => {});
  server.kill('SIGTERM');
  await new Promise((resolve) => setTimeout(resolve, 250));
  if (server.exitCode === null) server.kill('SIGKILL');
  await fs.rm(tempRoot, { recursive: true, force: true }).catch(() => {});
}
