import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { parse } from 'yaml';

export async function verifyResults(results, baseline, revision) {
  const requirements = parse(await readFile(new URL(
    `./node_modules/@modelcontextprotocol/conformance/requirements/${revision}.yaml`, import.meta.url,
  ), 'utf8'));
  const expected = [...requirements.server, ...requirements.not_scored
    .filter((entry) => entry.leg === 'server').map((entry) => entry.scenario)];
  // alpha.11 uses raw HTTP/SSE in these scenarios without wire-schema
  // instrumentation. Keep this explicit so missing checks elsewhere fail.
  const uninstrumented = new Set(revision === '2025-11-25'
    ? ['server-sse-multiple-streams', 'dns-rebinding-protection',
      'server-session-lifecycle', 'server-sse-polling']
    : ['server-stateless', 'server-sse-multiple-streams', 'dns-rebinding-protection',
      'tasks-status-notifications']);
  // These skips are fixture limitations in the pinned suite. Any new skip
  // must be reviewed rather than silently reducing the coverage of a green run.
  const allowedSkips = new Set(revision === '2026-07-28' ? [
    'caching:sep-2549-resources-read-caching-hints',
    'tasks-status-notifications:tasks-status-notifications',
    'server-stateless:sep-2575-server-sends-prompts-list-changed-on-subscription',
  ] : []);
  const scenarios = new Map();
  for (const entry of await readdir(results, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const name = entry.name.match(/^server-(.+)-\d{4}-\d{2}-\d{2}T[\d-]+Z$/)?.[1];
    assert(expected.includes(name), `Unexpected result directory: ${entry.name}`);
    assert(!scenarios.has(name), `Duplicate scenario results: ${name}`);
    const checks = JSON.parse(await readFile(join(results, entry.name, 'checks.json'), 'utf8'));
    assert(Array.isArray(checks) && checks.length > 0, `Empty checks: ${name}`);
    if (revision === '2026-07-28' && name === 'tasks-status-notifications') {
      assert.deepEqual(checks.map(({ id, status }) => ({ id, status })),
        [{ id: name, status: 'SKIPPED' }],
        'Unexpected tasks-status-notifications result; review the upstream scenario');
    }
    for (const check of checks.filter((check) => check.status === 'SKIPPED')) {
      assert(allowedSkips.has(`${name}:${check.id}`),
        `Unexpected skipped check: ${name}:${check.id}`);
    }
    // Enforce these even in the manifest's unscored scenarios, whose failures
    // are otherwise excluded from the conformance CLI's exit status.
    const wire = checks.filter((check) => check.id === 'wire-schema-valid');
    if (wire.length === 0) {
      assert(uninstrumented.has(name), `Missing wire-schema validation: ${name}`);
    } else {
      assert(!uninstrumented.has(name), `Now instrumented, remove exemption: ${name}`);
    }
    for (const check of wire) {
      assert.equal(check.status, 'SUCCESS', `Wire-schema failure: ${name}`);
      assert(check.details?.messagesValidated > 0, `No wire messages validated: ${name}`);
    }
    for (const check of checks.filter((check) => check.id === 'wire-schema-harness-error')) {
      assert.equal(check.status, 'SUCCESS', `Invalid harness traffic: ${name}`);
    }
    scenarios.set(name, checks);
  }
  for (const name of expected) assert(scenarios.has(name), `Missing scenario results: ${name}`);

  // These currently pass but are unscored upstream. Preserve their signal
  // locally; the polling scenario's disconnect/resume warning remains a gap.
  for (const [scenario, ids] of (revision === '2025-11-25' ? [
    ['server-session-lifecycle', [
      'server-session-initialized-accepted', 'server-session-delete-accepted',
      'server-session-terminated-returns-404',
    ]],
    ['server-sse-polling', ['server-sse-priming-event', 'server-sse-retry-field']],
  ] : [])) {
    for (const id of ids) {
      assert.equal(scenarios.get(scenario).find((check) => check.id === id)?.status,
        'SUCCESS', `Supplemental transport check failed: ${scenario}:${id}`);
    }
  }

  if (revision === '2026-07-28') {
    const header = scenarios.get('http-header-validation');
    for (const [id, count] of [
      ['sep-2243-server-reject-invalid-headers', 5],
      ['sep-2243-server-reject-error-code', 5],
      ['sep-2243-header-name-case-insensitive', 2],
      ['sep-2243-server-accepts-whitespace-header-value', 1],
    ]) {
      const matches = header.filter((check) => check.id === id);
      assert.equal(matches.length, count, `Missing header check occurrence: ${id}`);
      for (const check of matches) {
        assert.equal(check.status, 'SUCCESS', `Header validation failed: ${id}:${check.name}`);
      }
    }
  }

  // The upstream runner tolerates absent baseline checks. Reject typos and
  // renamed checks locally so exceptions cannot silently stop being exercised.
  const exceptions = parse(await readFile(baseline, 'utf8')).server ?? [];
  const warnings = new Set(revision === '2026-07-28' ? [
    'sep-2164-resource-not-found:sep-2164-data-uri',
    'input-required-result-missing-input-response:sep-2322-missing-response-rerequests',
    'input-required-result-ignore-extra-params:sep-2322-ignore-unexpected-params',
  ] : []);
  assert.equal(new Set(exceptions).size, exceptions.length, 'Duplicate baseline entry');
  for (const entry of warnings) {
    assert(exceptions.includes(entry), `Missing warning baseline entry: ${entry}`);
  }
  for (const entry of exceptions) {
    assert(typeof entry === 'string' && entry.split(':').length === 2,
      `Baseline must name an individual scenario:check-id: ${entry}`);
    const [scenario, id] = entry.split(':');
    assert(!id.startsWith('wire-schema-'), 'Wire checks must never be baselined');
    assert(scenario !== 'caching', 'Caching checks must never be baselined');
    const checks = scenarios.get(scenario)?.filter((check) => check.id === id) ?? [];
    assert(checks.length > 0, `Absent baseline check: ${entry}`);
    const status = warnings.has(entry) ? 'WARNING' : 'FAILURE';
    for (const check of checks) {
      assert.equal(check.status, status,
        `Baseline status changed: ${entry} (expected ${status}, got ${check.status})`);
    }
  }

  function passed(name) {
    const check = scenarios.get(name).find((check) => check.id === name);
    assert.equal(check?.status, 'SUCCESS', `Required fixture check failed: ${name}`);
    return check.details;
  }
  const success = passed('tools-call-simple-text').result;
  assert.equal(success.isError, false, 'Text tool returned an error instead of GraphQL data');
  const successText = success.content.find((content) => content.type === 'text')?.text;
  assert.deepEqual(JSON.parse(successText), { data: { text: 'This is a simple text response for testing.' } });

  const error = passed('tools-call-error').result;
  assert.equal(error.isError, true);
  const errorText = error.content.find((content) => content.type === 'text')?.text;
  const graphqlError = JSON.parse(errorText);
  assert.deepEqual(graphqlError.data, { intentionalError: null });
  assert.equal(graphqlError.errors[0].message, 'This tool intentionally returns an error for testing');
  assert.deepEqual(graphqlError.errors[0].path, ['intentionalError']);

  passed('prompts-get-simple');
  assert.deepEqual(passed('prompts-get-with-args').messages, [{
    role: 'user', content: { type: 'text', text: "Prompt with arguments: arg1='testValue1', arg2='testValue2'" },
  }]);
  console.log(`Verified all ${expected.length} scenarios, available wire checks, and actual fixture results.`);
}
