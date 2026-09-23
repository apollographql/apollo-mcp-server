# Conformance coverage gaps

The baseline records missing fixture behavior, not proven specification
violations. A successful optional workflow means no unexpected regression in
the checks we exercise. It does not establish full protocol conformance.

## Unsupported fixture behavior requiring broader work

These 17 required scenarios remain baselined at individual-check granularity.
Making them pass through the production server requires capability or public
configuration design, not just renaming the two GraphQL operations. Decide on
product value before expanding the server solely to satisfy these fixtures.

| Scenarios | Current limitation | Work needed to remove the baseline |
| --- | --- | --- |
| `tools-call-image`, `tools-call-audio`, `tools-call-embedded-resource`, `tools-call-mixed-content` | This fixture's GraphQL tools return JSON as text, not the requested MCP content blocks. | Design how GraphQL output maps to typed MCP content, including MIME types and encoding, and exercise that production path. |
| `tools-call-with-logging`, `tools-call-with-progress` | The configured operations do not emit the requested protocol notifications. Server logs alone do not satisfy these checks. | Provide a supported way to emit request-related MCP logging/progress from operation execution and a deterministic fixture. |
| `tools-call-sampling`, `tools-call-elicitation`, `elicitation-sep1034-defaults`, `elicitation-sep1330-enums` | The fixture cannot initiate the requested client interactions. | Design client capability handling and the operation lifecycle for sampling/elicitation, including cancellation, validation, and failure handling. |
| `resources-read-text`, `resources-read-binary`, `resources-templates-read` | Resources are HTML MCP Apps routed by app name; the suite hardcodes generic `test://` resources. | Generic resource/template support or upstream fixture parameterization. Actual Apollo HTML listing/read/error behavior is covered separately in `supplemental.mjs`; binary and template behavior is not. |
| `resources-subscribe`, `resources-unsubscribe` | The required 2025 resource subscription fixture is unavailable. | Define resource update ownership and notification lifecycle for that protocol revision. Future-version subscription work does not automatically satisfy these dated scenarios. |
| `prompts-get-embedded-resource`, `prompts-get-with-image` | Markdown prompt templates produce text messages. | Design typed prompt content and configuration, then add representative fixtures. |

A separate fake MCP server or test-only handlers could satisfy more fixtures,
but would not establish that the shipped Apollo server supports those paths.
The current workflow deliberately runs the production binary.

## Upstream and pending coverage limitations

- `server-sse-multiple-streams`, `dns-rebinding-protection`,
  `server-session-lifecycle`, and `server-sse-polling` do not emit
  `wire-schema-valid` in alpha.11. Full wire coverage for these scenarios needs
  upstream instrumentation or a suite upgrade. SDK validation in our separate
  resource checks does not fill that gap. All wire checks that are emitted
  remain mandatory and cannot be baselined.
- `server-session-lifecycle` is unscored upstream. The local verifier explicitly
  enforces acceptance of initialization, session deletion, and rejection of a
  terminated session, so a regression fails this workflow.
- `server-sse-polling` is pending/unscored. Its priming and retry-field checks
  pass and are enforced locally. Its disconnect/resume check currently warns:
  the `test_reconnection` tool is absent and no tool result is received after
  reconnecting. This is **not successful reconnection coverage**. A meaningful
  fixture needs deterministic mid-call disconnection and resumed delivery;
  simply adding a fast GraphQL tool with that name would not exercise it.
- `json-schema-2020-12` is pending/unscored and fails because its special tool
  fixture is absent. Arbitrary JSON Schema 2020-12 fixture constructs are not
  exposed by this GraphQL fixture. Assess the relevant GraphQL-to-JSON-Schema
  mappings and upstream scenario maturity before adding coverage; do not infer
  support from the suite's exit status.

## Operational scope

- Manual execution is intentional. Run after rmcp upgrades, MCP handler or
  transport changes, and suite updates; this does not detect every PR regression.
- Validate workflow changes on a GitHub-hosted runner before merge and link the
  run in the PR. Local success alone does not verify runner-specific behavior.
- The suite and supplemental SDK are pinned exactly. The supplemental checks
  assert negotiation of `2025-11-25`. When AMS-507 enables `2026-07-28`, add its
  own requirements run and baseline and review supplemental lifecycle/error
  assertions for the new version.
