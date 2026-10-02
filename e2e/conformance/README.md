# MCP server conformance

This is an optional, manually triggered check. In GitHub Actions, select
**MCP conformance**, choose **Run workflow**, and select the branch to test.
The workflow becomes available in the Actions UI once it is on the default
branch. Run it when upgrading rmcp, changing MCP handlers or transport behavior,
or updating the conformance suite. It does not run automatically on pull requests
or pushes and should not be configured as a required branch-protection check.
Both `2025-11-25` and `2026-07-28` run as separate jobs. Download the matching
`mcp-conformance-<revision>` artifact to inspect results and logs.

Once GitHub has registered the workflow, it can also be dispatched against a
pushed branch from the CLI:

```bash
gh workflow run conformance.yml --ref <branch>
```

Alternatively, run from the repository root:

```bash
bash e2e/conformance/run.sh
bash e2e/conformance/run.sh --revision 2026-07-28
```

The command installs the exact package versions in `package-lock.json`, builds
the production `apollo-mcp-server` binary, and starts it with local schema,
operations, prompts, and a GraphQL endpoint. It runs
`@modelcontextprotocol/conformance@0.2.0-alpha.11 server --requirements
<revision>`. The default is `2025-11-25`; only these two revisions are accepted.
No external API, credentials, or production code changes are
needed. Node 22 and the repository's Rust toolchain are required. Ports 4100
and 4101 on loopback must be available.

The workflow invokes the pinned official CLI rather than the upstream composite
GitHub Action because that action does not currently expose a `requirements`
input. This preserves revision-specific testing.

Each run writes `artifacts/run-*/` with the suite's per-scenario `checks.json`,
suite output and server logs. The legacy run also writes supplemental results
in `apollo-content.json`.
CI uploads this directory even when the job
fails. The runner stops its processes when it exits or receives a signal.

The `2025-11-25` requirements manifest selects 30 scored server scenarios and
three unscored scenarios. On 2026-09-18, 13 scored scenarios passed. The 17
scored failures require fixture behaviors that Apollo MCP Server cannot create
through its existing configuration, such as image/audio tool content, generic
`test://` resources, subscriptions, and prompts with non-text content. Each
failing check is recorded in `expected-failures-2025-11-25.yaml`; this does not
mean the product is fully conformant. The pending `json-schema-2020-12`
scenario also fails because its fixture tool is absent, but the suite reports
it without scoring it.

The `2026-07-28` manifest selects 37 scored server scenarios and 13 unscored
scenarios. Its baseline lists 24 failed scored checks requiring unavailable
fixture behavior and four check-level warnings. The pinned CLI treats warnings
as baseline failures; the verifier requires those four checks to remain warnings
so a new failure or resolved warning is noticed. The unscored task extension, JSON Schema, and custom-header scenarios
remain visible. The verifier requires every repeated check in the unscored
`http-header-validation` scenario to pass. `tasks-status-notifications` emits one
`SKIPPED` check pending an upstream rewrite. Five custom-header failures lack a
production fixture. The modern resource-read cache check is skipped by the
suite, so the run does not prove read caching. This fixture also lacks a
tool-list-change trigger. The other warnings cover a SHOULD-level resource error
URI and two input-required-result recommendations whose fixture tools are absent.

The passing content checks exercise Apollo's GraphQL execution, error mapping,
and prompt rendering, providing application-level signal beyond rmcp's own
conformance tests. For the legacy revision, supplemental SDK checks initialize a real session, list and
read a local MCP App resource, compare the exact HTML and MIME type, check the
missing-resource error, verify simple prompt text, and terminate the session.
They use Apollo's supported `ui://` URI and app routing; they do not change the
upstream score or turn its `test://` resource scenarios into passes.
A green run means the current baseline holds,
not that all required scenarios pass. Manual execution also means regressions
are detected only when someone runs this workflow.

The runner requires every scenario result, enforces every available
`wire-schema-valid` check, and inspects the actual GraphQL success and error
responses. The pinned suite does not produce wire-schema checks for four legacy
scenarios, or for modern `server-stateless`, `server-sse-multiple-streams`,
`dns-rebinding-protection`, and the skipped task scenario. The verifier lists
these exceptions explicitly.
For `2025-11-25`, it also requires the three unscored session-lifecycle checks and the two SSE
priming/retry checks to keep passing, since upstream does not enforce them.
The suite fails on new failures, warnings, or stale baseline entries. When updating a
baseline, inspect the relevant `checks.json`, keep exceptions at
`scenario:check-id` granularity, and rerun the command. The runner rejects
missing check IDs and baseline entries for wire-schema checks.

Remaining fixture and upstream gaps are tracked in [GAPS.md](GAPS.md), including
the work needed to close them. None is silently treated as full coverage.

For a baseline experiment without editing the repository, pass a YAML file to
the runner:

```bash
cd e2e/conformance
npm test -- /path/to/experimental-baseline.yaml
npm test -- --revision 2026-07-28 /path/to/modern-baseline.yaml
```

Both revisions run independently because their protocol lifecycles differ.
