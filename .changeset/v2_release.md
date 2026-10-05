---
default: major
---

# Apollo MCP Server 2.0

Apollo MCP Server 2.0 adds support for MCP protocol `2026-07-28` while continuing to serve clients on earlier protocol versions. The major version reflects how much the MCP specification and the server have changed since 1.0.

Highlights since 1.0:

- MCP `2025-11-25` (1.16.0) and `2026-07-28` (2.0.0) support, including `server/discover`, `subscriptions/listen`, cache hints, and standard request headers. Tool output schemas and structured content arrived in 1.4.0.
- MCP Apps and OpenAI Apps SDK support (1.8.0), and prompts defined in Markdown files (1.13.0).
- Rhai scripting for request hooks (1.10.0), with hot reloading for scripts (1.11.0) and the config file (1.12.0).
- OAuth scope enforcement with `insufficient_scope` (1.4.0), metadata discovery (1.6.0), step-up authorization (1.10.0), issuer validation (1.15.0, 1.17.0), per-operation scope alternatives, and configurable token validation skips (1.18.0).
- Header forwarding to the GraphQL API (1.1.0) and distributed trace context propagation (1.4.0, 2.0.0).

Upgrading from 1.20.0 doesn't require configuration changes. Review Rhai scripts that write to variables declared at the top level of `main.rhai`, since each hook invocation now gets its own copy of those variables.

If you're upgrading from an earlier 1.x release, these changes may also need attention:

- 1.1.0: The default port changed from `5000` to `8000`.
- 1.5.0: The SSE transport was removed. Use `streamable_http`.
- 1.6.0: GraphQL API failures and input validation errors are returned as tool execution errors with `isError: true`.
- 1.7.0: Host header validation is enabled by default for `streamable_http`. Add hostnames other than localhost to `transport.host_validation.allowed_hosts`.
- 1.7.0 and 1.17.0: Invalid or misplaced configuration, such as `auth` at the top level or under `stdio`, fails at startup instead of being ignored.
- 1.14.0: `transport.auth.servers` entries are published as written, so each must exactly match its authorization server's `issuer`.
- 1.18.0: Nullable inputs in generated tool schemas use `anyOf` with a `null` alternative. `allow_anonymous_mcp_discovery` is deprecated in favor of `transport.auth.skip_token_validation.methods`.
- 1.20.0: Schemas are validated against the GraphQL September 2025 rules, so a schema that loaded before can be rejected at startup.
