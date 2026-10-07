import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { parse, stringify } from 'yaml';
import { verifyResults } from './verify.mjs';

const revision = '2026-07-28';
const baseline = parse(await readFile(new URL('./expected-failures-2026-07-28.yaml', import.meta.url), 'utf8'));
const warningEntries = new Set([
  'sep-2164-resource-not-found:sep-2164-data-uri',
  'input-required-result-missing-input-response:sep-2322-missing-response-rerequests',
  'input-required-result-ignore-extra-params:sep-2322-ignore-unexpected-params',
]);
const noWire = new Set(['server-stateless', 'server-sse-multiple-streams',
  'dns-rebinding-protection', 'tasks-status-notifications']);

async function fixture(t, fixtureRevision = revision) {
  const requirements = parse(await readFile(new URL(
    `./node_modules/@modelcontextprotocol/conformance/requirements/${fixtureRevision}.yaml`, import.meta.url,
  ), 'utf8'));
  const fixtureBaseline = parse(await readFile(new URL(
    `./expected-failures-${fixtureRevision}.yaml`, import.meta.url,
  ), 'utf8'));
  const scenarios = [...requirements.server, ...requirements.not_scored
    .filter((entry) => entry.leg === 'server').map((entry) => entry.scenario)];
  const uninstrumented = fixtureRevision === revision ? noWire : new Set([
    'server-sse-multiple-streams', 'dns-rebinding-protection',
    'server-session-lifecycle', 'server-sse-polling',
  ]);
  const root = await mkdtemp(join(tmpdir(), 'conformance-verify-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const results = join(root, 'results');
  await mkdir(results);
  const files = new Map();
  for (const scenario of scenarios) {
    const directory = join(results, `server-${scenario}-2026-09-29T00-00-00Z`);
    await mkdir(directory);
    const checks = scenario === 'tasks-status-notifications'
      ? [{ id: scenario, status: 'SKIPPED' }]
      : [{ id: scenario, status: 'SUCCESS' }];
    if (!uninstrumented.has(scenario)) checks.push({
      id: 'wire-schema-valid', status: 'SUCCESS', details: { messagesValidated: 1 },
    });
    files.set(scenario, { directory, checks });
  }
  const textResult = { content: [{ type: 'text', text: JSON.stringify({
    data: { text: 'This is a simple text response for testing.' },
  }) }], isError: false };
  files.get('tools-call-simple-text').checks[0].details = { result: textResult };
  files.get('tools-call-error').checks[0].details = { result: {
    isError: true, content: [{ type: 'text', text: JSON.stringify({
      data: { intentionalError: null },
      errors: [{ message: 'This tool intentionally returns an error for testing', path: ['intentionalError'] }],
    }) }],
  } };
  files.get('prompts-get-with-args').checks[0].details = { messages: [{
    role: 'user', content: { type: 'text', text: "Prompt with arguments: arg1='testValue1', arg2='testValue2'" },
  }] };
  const header = files.get('http-header-validation')?.checks;
  for (const [id, count] of [
    ['sep-2243-server-reject-invalid-headers', 5],
    ['sep-2243-server-reject-error-code', 5],
    ['sep-2243-header-name-case-insensitive', 2],
    ['sep-2243-server-accepts-whitespace-header-value', 1],
  ]) {
    for (let i = 0; header && i < count; i++) header.push({ id, name: `${id}-${i}`, status: 'SUCCESS' });
  }
  if (fixtureRevision === '2025-11-25') {
    for (const [scenario, ids] of [
      ['server-session-lifecycle', [
        'server-session-initialized-accepted', 'server-session-delete-accepted',
        'server-session-terminated-returns-404',
      ]],
      ['server-sse-polling', ['server-sse-priming-event', 'server-sse-retry-field']],
    ]) {
      files.get(scenario).checks.push(...ids.map((id) => ({ id, status: 'SUCCESS' })));
    }
  }
  for (const entry of fixtureBaseline.server) {
    const [scenario, id] = entry.split(':');
    const checks = files.get(scenario).checks;
    const existing = checks.find((check) => check.id === id);
    const status = warningEntries.has(entry) ? 'WARNING' : 'FAILURE';
    if (existing) existing.status = status;
    else checks.push({ id, status });
  }
  const baselinePath = join(root, 'baseline.yaml');
  await writeFile(baselinePath, stringify(fixtureBaseline));
  async function save() {
    for (const { directory, checks } of files.values()) {
      await writeFile(join(directory, 'checks.json'), JSON.stringify(checks));
    }
  }
  function verify() { return verifyResults(results, baselinePath, fixtureRevision); }
  await save();
  return { files, save, verify, baselinePath, baseline: fixtureBaseline };
}

test('modern verifier accepts complete fixture', async (t) => {
  const f = await fixture(t);
  await f.verify();
});

test('modern verifier accepts the documented fixture skips', async (t) => {
  const f = await fixture(t);
  f.files.get('caching').checks.push({
    id: 'sep-2549-resources-read-caching-hints', status: 'SKIPPED',
  });
  f.files.get('server-stateless').checks.push({
    id: 'sep-2575-server-sends-prompts-list-changed-on-subscription', status: 'SKIPPED',
  });
  await f.save();
  await f.verify();
});

test('modern verifier rejects unexpected skips even with successful wire validation', async (t) => {
  const f = await fixture(t);
  f.files.get('completion-complete').checks[0].status = 'SKIPPED';
  await f.save();
  await assert.rejects(f.verify(), /Unexpected skipped check: completion-complete:completion-complete/);
});

test('modern verifier rejects a later repeated header failure', async (t) => {
  const f = await fixture(t);
  const checks = f.files.get('http-header-validation').checks;
  checks.filter((check) => check.id === 'sep-2243-server-reject-invalid-headers')[4].status = 'FAILURE';
  await f.save();
  await assert.rejects(f.verify(), /Header validation failed/);
});

test('modern verifier rejects a missing repeated header occurrence', async (t) => {
  const f = await fixture(t);
  const checks = f.files.get('http-header-validation').checks;
  checks.splice(checks.findLastIndex((check) => check.id === 'sep-2243-server-reject-error-code'), 1);
  await f.save();
  await assert.rejects(f.verify(), /Missing header check occurrence/);
});

test('modern verifier rejects a missing scenario', async (t) => {
  const f = await fixture(t);
  await rm(f.files.get('completion-complete').directory, { recursive: true });
  await assert.rejects(f.verify(), /Missing scenario results/);
});

test('modern verifier rejects failed wire checks', async (t) => {
  const f = await fixture(t);
  f.files.get('tools-list').checks.find((check) => check.id === 'wire-schema-valid').status = 'FAILURE';
  await f.save();
  await assert.rejects(f.verify(), /Wire-schema failure/);
});

test('modern verifier rejects absent and stale baseline checks', async (t) => {
  const f = await fixture(t);
  const absent = { server: [...baseline.server, 'tools-list:no-such-check'] };
  await writeFile(f.baselinePath, stringify(absent));
  await assert.rejects(f.verify(), /Absent baseline check/);
  await writeFile(f.baselinePath, stringify(baseline));
  const [scenario, id] = baseline.server[0].split(':');
  f.files.get(scenario).checks.find((check) => check.id === id).status = 'SUCCESS';
  await f.save();
  await assert.rejects(f.verify(), /Baseline status changed/);
});

test('modern verifier rejects baselined caching failures', async (t) => {
  const f = await fixture(t);
  const id = 'sep-2549-resources-templates-list-caching-hints';
  f.files.get('caching').checks.push({ id, status: 'FAILURE' });
  await f.save();
  await writeFile(f.baselinePath, stringify({ server: [...baseline.server, `caching:${id}`] }));
  await assert.rejects(f.verify(), /Caching checks must never be baselined/);
});

test('modern verifier rejects warning promotion to failure or success', async (t) => {
  const f = await fixture(t);
  const entry = [...warningEntries][0];
  const [scenario, id] = entry.split(':');
  const check = f.files.get(scenario).checks.find((item) => item.id === id);
  for (const status of ['FAILURE', 'SUCCESS']) {
    check.status = status;
    await f.save();
    await assert.rejects(f.verify(), /Baseline status changed/);
  }
});

test('legacy verifier accepts complete fixture', async (t) => {
  const f = await fixture(t, '2025-11-25');
  await f.verify();
});

test('legacy verifier rejects any skipped check', async (t) => {
  const f = await fixture(t, '2025-11-25');
  f.files.get('completion-complete').checks[0].status = 'SKIPPED';
  await f.save();
  await assert.rejects(f.verify(), /Unexpected skipped check/);
});

test('legacy verifier rejects duplicate baseline entries', async (t) => {
  const f = await fixture(t, '2025-11-25');
  await writeFile(f.baselinePath, stringify({ server: [...f.baseline.server, f.baseline.server[0]] }));
  await assert.rejects(f.verify(), /Duplicate baseline entry/);
});

test('legacy verifier checks every repeated baseline instance', async (t) => {
  const f = await fixture(t, '2025-11-25');
  const [scenario, id] = f.baseline.server[0].split(':');
  const checks = f.files.get(scenario).checks;
  const later = { id, status: 'FAILURE' };
  checks.push(later);
  await f.save();
  await f.verify();
  for (const status of ['SUCCESS', 'WARNING']) {
    later.status = status;
    await f.save();
    await assert.rejects(f.verify(), /Baseline status changed/);
  }
});

test('runner rejects unsupported or ambiguous arguments before setup', () => {
  for (const args of [
    ['--revision', '2024-11-05'],
    ['--revision'],
    ['--revision', revision, '--revision', revision],
    ['first.yaml', 'second.yaml'],
  ]) {
    const result = spawnSync(process.execPath, [new URL('./run.mjs', import.meta.url).pathname,
      ...args], { encoding: 'utf8', timeout: 10_000 });
    assert.ifError(result.error);
    assert.equal(result.signal, null);
    assert.notEqual(result.status, 0);
    assert.doesNotMatch(result.stdout, /Conformance artifacts:/);
  }
});
