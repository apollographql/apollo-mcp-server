import { spawn, execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { openSync, closeSync } from 'node:fs';
import { mkdir, mkdtemp, readFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const fixture = dirname(fileURLToPath(import.meta.url));
const repo = resolve(fixture, '../..');
const keepBackends = process.argv.includes('--keep');
const MCP = 'http://127.0.0.1:4200/mcp';
const JAEGER = 'http://127.0.0.1:16686';
const PROMETHEUS = 'http://127.0.0.1:9090';
const PROTOCOL = '2026-07-28';
const LEGACY_PROTOCOL = '2025-11-25';

await mkdir(join(fixture, 'artifacts'), { recursive: true });
const artifacts = await mkdtemp(join(fixture, 'artifacts/run-'));
console.log(`Telemetry artifacts: ${artifacts}`);

// Local credentials/configuration must not change this deterministic fixture.
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('APOLLO_')));
const children = [];
function start(command, args, options = {}) {
  const child = spawn(command, args, { cwd: fixture, env, ...options });
  const task = { child, stopped: false };
  task.done = new Promise((resolve) => {
    child.on('error', (error) => {
      console.error(error);
      task.stopped = true;
      resolve(1);
    });
    child.on('close', (code) => {
      task.stopped = true;
      resolve(code ?? 1);
    });
  });
  children.push(task);
  return task;
}
function logged(command, args, name, extraEnv = {}) {
  const fd = openSync(join(artifacts, name), 'w');
  try {
    return start(command, args, { stdio: ['ignore', fd, fd], env: { ...env, ...extraEnv } });
  } finally {
    closeSync(fd);
  }
}
async function stop(task) {
  if (task.stopped) return;
  task.child.kill('SIGTERM');
  const kill = setTimeout(() => task.child.kill('SIGKILL'), 5000);
  try { await task.done; } finally { clearTimeout(kill); }
}
async function cleanup() {
  await Promise.all(children.map(stop));
  if (!keepBackends) compose('down');
}
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, async () => {
    await cleanup();
    process.exit(signal === 'SIGINT' ? 130 : 143);
  });
}
function compose(...args) {
  execFileSync('docker', ['compose', ...args], { cwd: fixture, stdio: 'inherit' });
}
async function poll(what, check, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch { /* Not ready yet. */ }
    await delay(500);
  }
  throw new Error(`Timed out waiting for ${what}`);
}
async function ready(task, url) {
  // `poll` retries on errors, so an exit is signaled through the return value.
  const exited = await poll(url, async () => {
    if (task.stopped) return 'exited';
    const response = await fetch(url, { signal: AbortSignal.timeout(1000) });
    await response.arrayBuffer();
    return response.ok;
  });
  if (exited === 'exited') throw new Error(`Process exited before readiness: ${url}`);
}

// `TraceIdRatioBased` keeps a trace when the low 64 bits of its ID, shifted
// right by one, fall below the ratio. Fixing those bits makes a 0.5 ratio's
// decision predictable.
function traceId(ratioKeeps) {
  return randomBytes(8).toString('hex') + (ratioKeeps ? '0'.repeat(16) : 'f'.repeat(16));
}
function spanId() {
  return randomBytes(8).toString('hex');
}
function traceparent(trace, span, sampled) {
  return `00-${trace}-${span}-${sampled ? '01' : '00'}`;
}

let requestId = 0;
async function post(body, headers) {
  const response = await fetch(MCP, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      accept: 'application/json, text/event-stream',
      ...headers,
    },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${body.method} returned ${response.status}: ${text}`);
  return response.headers;
}

/** Sends one stateless MCP request, with trace context in the header, `_meta`, or both. */
async function mcp(method, params, { header, meta } = {}) {
  const body = {
    jsonrpc: '2.0',
    id: ++requestId,
    method,
    params: {
      ...params,
      _meta: {
        'io.modelcontextprotocol/protocolVersion': PROTOCOL,
        'io.modelcontextprotocol/clientCapabilities': {},
        ...(meta && { traceparent: meta }),
      },
    },
  };
  await post(body, {
    'mcp-protocol-version': PROTOCOL,
    'mcp-method': method,
    'mcp-name': params.name ?? '',
    ...(header && { traceparent: header }),
  });
}

/** Opens a legacy-protocol session and returns the headers for requests on it. */
async function legacySession() {
  const headers = await post({
    jsonrpc: '2.0',
    id: ++requestId,
    method: 'initialize',
    params: {
      protocolVersion: LEGACY_PROTOCOL,
      capabilities: {},
      clientInfo: { name: 'telemetry-e2e', version: '1.0.0' },
    },
  }, {});
  const session = { 'mcp-session-id': headers.get('mcp-session-id'), 'mcp-protocol-version': LEGACY_PROTOCOL };
  await post({ jsonrpc: '2.0', method: 'notifications/initialized' }, session);
  return session;
}

/** The trace's spans in Jaeger, or `undefined` when Jaeger doesn't have it. */
async function jaegerTrace(id) {
  const response = await fetch(`${JAEGER}/api/traces/${id}`);
  if (response.status === 404) return undefined;
  // Any other error must not read as a missing trace, which a sampling check
  // would count as dropped.
  if (!response.ok) throw new Error(`Jaeger returned ${response.status} for trace ${id}`);
  const { data } = await response.json();
  return data?.[0]?.spans;
}
async function waitForTrace(id) {
  return poll(`trace ${id} in Jaeger`, () => jaegerTrace(id));
}
async function prometheus(query) {
  const url = `${PROMETHEUS}/api/v1/query?query=${encodeURIComponent(query)}`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`Prometheus returned ${response.status}: ${await response.text()}`);
  const { data } = await response.json();
  return data.result;
}

const named = (spans, name) => spans.find((span) => span.operationName === name);
const tag = (span, key) => span?.tags.find((t) => t.key === key)?.value;
const reference = (span, type) => span?.references.find((r) => r.refType === type);

const results = [];
function check(name, ok, detail = '') {
  const passed = Boolean(ok);
  results.push({ name, ok: passed });
  const note = passed || !detail ? '' : ` (${detail})`;
  console.log(`${passed ? 'PASS' : 'FAIL'} ${name}${note}`);
}

async function startServer(name, extraEnv = {}) {
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: repo, encoding: 'utf8' }));
  const server = logged(join(metadata.target_directory, 'debug/apollo-mcp-server'), ['config.yaml'], name, extraEnv);
  await ready(server, 'http://127.0.0.1:4200/health');
  return server;
}

/** With `parent_based_sampler` on, the caller's sampled flag overrides the ratio. */
async function parentBasedRun() {
  const server = await startServer('server.log');

  const unsampled = traceId(true);
  await mcp('tools/call', { name: 'Hello', arguments: {} }, { header: traceparent(unsampled, spanId(), false) });

  const viaHeader = { trace: traceId(false), span: spanId() };
  await mcp('tools/call', { name: 'Hello', arguments: {} }, { header: traceparent(viaHeader.trace, viaHeader.span, true) });

  const viaMeta = { trace: traceId(false), span: spanId() };
  const transport = { trace: traceId(false), span: spanId() };
  await mcp('tools/call', { name: 'Hello', arguments: {} }, {
    header: traceparent(transport.trace, transport.span, true),
    meta: traceparent(viaMeta.trace, viaMeta.span, true),
  });

  // `logging/setLevel` logs inside its span, but only on the legacy protocol:
  // at 2026-07-28 the method no longer exists.
  const logTrace = traceId(false);
  await post(
    { jsonrpc: '2.0', id: ++requestId, method: 'logging/setLevel', params: { level: 'debug' } },
    { ...(await legacySession()), traceparent: traceparent(logTrace, spanId(), true) },
  );

  // Header context: the HTTP span continues the caller's span, and the
  // handler and downstream client spans nest under it.
  const headerSpans = await waitForTrace(viaHeader.trace);
  const http = named(headerSpans, 'POST /mcp');
  const callTool = named(headerSpans, 'tools/call Hello');
  const client = named(headerSpans, 'mcp-graphql-client');
  check('header traceparent parents the HTTP span', reference(http, 'CHILD_OF')?.spanID === viaHeader.span);
  check('handler span nests under the HTTP span', reference(callTool, 'CHILD_OF')?.spanID === http?.spanID);
  check('GraphQL client span joins the trace', client !== undefined);
  for (const [key, expected] of [
    ['mcp.method.name', 'tools/call'],
    ['gen_ai.operation.name', 'execute_tool'],
    ['gen_ai.tool.name', 'Hello'],
    ['mcp.protocol.version', PROTOCOL],
    ['network.transport', 'tcp'],
  ]) {
    check(`handler span has ${key}=${expected}`, tag(callTool, key) === expected, `got ${tag(callTool, key)}`);
  }
  check('handler span kind is SERVER', tag(callTool, 'span.kind') === 'server', `got ${tag(callTool, 'span.kind')}`);
  const graphqlLog = await readFile(join(artifacts, 'graphql.log'), 'utf8');
  check('traceparent reaches the GraphQL endpoint', graphqlLog.includes(viaHeader.trace));

  // `_meta` context: it parents the handler span, which links to the HTTP span.
  const metaSpans = await waitForTrace(viaMeta.trace);
  const metaHandler = named(metaSpans, 'tools/call Hello');
  check('_meta traceparent parents the handler span', reference(metaHandler, 'CHILD_OF')?.spanID === viaMeta.span);
  const transportSpans = await waitForTrace(transport.trace);
  const transportHttp = named(transportSpans, 'POST /mcp');
  const link = reference(metaHandler, 'FOLLOWS_FROM');
  check('handler span links to its HTTP span',
    link?.traceID === transport.trace && link?.spanID === transportHttp?.spanID);

  // Sampling: everything above was sent with IDs the ratio drops.
  check('a sampled traceparent overrides the ratio', headerSpans.length > 0);
  await waitForTrace(logTrace);
  check('an unsampled traceparent overrides the ratio', (await jaegerTrace(unsampled)) === undefined);

  // Log correlation: a JSON log line's `trace_id` finds its trace.
  const serverLog = (await readFile(join(artifacts, 'server.log'), 'utf8')).split('\n');
  const correlated = serverLog
    .filter((line) => line.startsWith('{'))
    .map((line) => JSON.parse(line))
    .some((line) => line.span === 'set_level' && line.trace_id === logTrace);
  check('JSON log trace_id matches an exported trace', correlated);

  // Metrics: the collector translates OTel names for Prometheus.
  const duration = await poll('mcp.server.operation.duration in Prometheus',
    () => prometheus('mcp_server_operation_duration_seconds_count{mcp_method_name="tools/call"}')
      .then((series) => series.length > 0 && series));
  check('mcp.server.operation.duration arrives', duration.length > 0);
  check('operation duration carries gen_ai.tool.name', duration.some((s) => s.metric.gen_ai_tool_name === 'Hello'));
  const apollo = await prometheus('{__name__=~"apollo_mcp_.*"}');
  check('apollo.mcp.* metrics arrive', apollo.length > 0);
  const httpMetrics = await prometheus('{__name__=~"http_server_request_duration_seconds.*"}');
  check('HTTP server metrics arrive', httpMetrics.length > 0);

  await stop(server);
}

/** With `parent_based_sampler: false`, the ratio decides whatever the caller says. */
async function ratioOnlyRun() {
  const server = await startServer('server-ratio-only.log', {
    APOLLO_MCP_TELEMETRY__EXPORTERS__TRACING__PARENT_BASED_SAMPLER: 'false',
  });
  const dropped = traceId(false);
  await mcp('tools/list', {}, { header: traceparent(dropped, spanId(), true) });
  const kept = traceId(true);
  await mcp('tools/list', {}, { header: traceparent(kept, spanId(), false) });

  await waitForTrace(kept);
  check('ratio keeps a trace the caller left unsampled', true);
  check('ratio drops a trace the caller sampled', (await jaegerTrace(dropped)) === undefined);
  await stop(server);
}

try {
  compose('up', '-d', '--wait');
  const build = start('cargo', ['build', '--locked', '-p', 'apollo-mcp-server'], { cwd: repo, stdio: 'inherit' });
  if (await build.done !== 0) throw new Error('Cargo build failed');
  const graphql = logged(process.execPath, ['graphql.mjs'], 'graphql.log');
  await ready(graphql, 'http://127.0.0.1:4201/health');

  await parentBasedRun();
  await ratioOnlyRun();

  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} telemetry checks passed.`);
  if (failed.length > 0) process.exitCode = 1;
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  if (process.exitCode) console.error(`See logs in ${artifacts}`);
  await cleanup();
}
