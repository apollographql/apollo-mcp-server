# MCP server conformance

This is an optional, manually triggered check. In GitHub Actions, select
**MCP conformance**, choose **Run workflow**, and select the branch to test.
The workflow becomes available in the Actions UI once it is on the default
branch. Run it when upgrading rmcp, changing MCP handlers or transport behavior,
or updating the conformance suite. It does not run automatically on pull requests
or pushes and should not be configured as a required branch-protection check.
Download the `mcp-conformance-2025-11-25` artifact from the workflow run to inspect
results and logs.

Once GitHub has registered the workflow, it can also be dispatched against a
pushed branch from the CLI:

```bash
gh workflow run conformance.yml --ref <branch>
```

Alternatively, run from the repository root:

```bash
bash e2e/conformance/run.sh
```

The command installs the exact package versions in `package-lock.json`, builds
the production `apollo-mcp-server` binary, and starts it with local schema,
operations, prompts, and a GraphQL endpoint. It runs
`@modelcontextprotocol/conformance@0.2.0-alpha.11 server --requirements
2025-11-25`. No external API, credentials, or production code changes are
needed. Node 22 and the repository's Rust toolchain are required. Ports 4100
and 4101 on loopback must be available.

The workflow invokes the pinned official CLI rather than the upstream composite
GitHub Action because that action does not currently expose a `requirements`
input. This preserves revision-specific testing.

Each run writes `artifacts/run-*/` with the suite's per-scenario `checks.json`,
suite output, server logs, and supplemental results in `apollo-content.json`.
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

The passing content checks exercise Apollo's GraphQL execution, error mapping,
and prompt rendering, providing application-level signal beyond rmcp's own
conformance tests. Supplemental SDK checks initialize a real session, list and
read a local MCP App resource, compare the exact HTML and MIME type, check the
missing-resource error, verify simple prompt text, and terminate the session.
They use Apollo's supported `ui://` URI and app routing; they do not change the
upstream score or turn its `test://` resource scenarios into passes.
A green run means the current baseline holds,
not that all required scenarios pass. Manual execution also means regressions
are detected only when someone runs this workflow.

The runner requires every scenario result, enforces every available
`wire-schema-valid` check, and inspects the actual GraphQL success and error
responses. The pinned suite does not produce wire-schema checks for four
scenarios that use raw HTTP or SSE; the verifier lists those explicitly.
It also requires the three unscored session-lifecycle checks and the two SSE
priming/retry checks to keep passing, since upstream does not enforce them.
The suite fails on new failures or stale baseline entries. When updating a
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
```

Once Apollo MCP Server supports `2026-07-28`, add a separate requirements run
and baseline for that revision. The requirement sets have different protocol
lifecycles and must be tested independently.
