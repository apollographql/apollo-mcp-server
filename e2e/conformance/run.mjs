import { spawn, execFileSync } from 'node:child_process';
import { openSync, closeSync } from 'node:fs';
import { mkdir, mkdtemp, readFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { verifyResults } from './verify.mjs';
import { verifyApolloContent } from './supplemental.mjs';

const fixture = dirname(fileURLToPath(import.meta.url));
const repo = resolve(fixture, '../..');
const revision = '2025-11-25';
const baseline = resolve(process.argv[2] ?? join(fixture, `expected-failures-${revision}.yaml`));
await mkdir(join(fixture, 'artifacts'), { recursive: true });
const artifacts = await mkdtemp(join(fixture, 'artifacts/run-'));
console.log(`Conformance artifacts: ${artifacts}`);

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
function logged(command, args, name) {
  const fd = openSync(join(artifacts, name), 'w');
  try {
    return start(command, args, { stdio: ['ignore', fd, fd] });
  } finally {
    closeSync(fd);
  }
}
async function cleanup() {
  await Promise.all(children.map(async (task) => {
    if (task.stopped) return;
    task.child.kill('SIGTERM');
    const kill = setTimeout(() => task.child.kill('SIGKILL'), 5000);
    try { await task.done; } finally { clearTimeout(kill); }
  }));
}
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, async () => {
    await cleanup();
    process.exit(signal === 'SIGINT' ? 130 : 143);
  });
}
async function ready(task, url) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (task.stopped) throw new Error(`Server exited before readiness: ${url}`);
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1000) });
      await response.arrayBuffer();
      if (response.ok && !task.stopped) return;
    } catch { /* Connection refused is normal while the process starts. */ }
    await delay(200);
  }
  throw new Error(`Timed out waiting for ${url}`);
}

try {
  const build = start('cargo', ['build', '--locked', '-p', 'apollo-mcp-server'], { cwd: repo, stdio: 'inherit' });
  if (await build.done !== 0) throw new Error('Cargo build failed');
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: repo, encoding: 'utf8' }));
  const graphql = logged(process.execPath, ['graphql.mjs'], 'graphql.log');
  await ready(graphql, 'http://127.0.0.1:4101/health');
  const server = logged(join(metadata.target_directory, 'debug/apollo-mcp-server'), ['config.yaml'], 'server.log');
  await ready(server, 'http://127.0.0.1:4100/health');
  await verifyApolloContent(artifacts);

  const results = join(artifacts, 'results');
  const suite = logged(process.execPath, [
    'node_modules/@modelcontextprotocol/conformance/dist/index.js', 'server',
    '--url', 'http://127.0.0.1:4100/mcp', '--requirements', revision,
    '--expected-failures', baseline, '--output-dir', results,
  ], 'conformance.log');
  const timeout = setTimeout(() => suite.child.kill('SIGKILL'), 300_000);
  let code;
  try { code = await suite.done; } finally { clearTimeout(timeout); }
  const log = await readFile(join(artifacts, 'conformance.log'), 'utf8');
  console.log(log.slice(Math.max(0, log.indexOf('=== SUMMARY ==='))));
  await verifyResults(results, baseline);
  if (graphql.stopped || server.stopped) throw new Error('A fixture process exited during the suite');
  if (code !== 0) throw new Error(`Conformance suite exited with ${code}`);
  console.log('Conformance baseline and GraphQL fixture assertions passed.');
} catch (error) {
  console.error(error);
  console.error(`See logs and results in ${artifacts}`);
  process.exitCode = 1;
} finally {
  await cleanup();
}
