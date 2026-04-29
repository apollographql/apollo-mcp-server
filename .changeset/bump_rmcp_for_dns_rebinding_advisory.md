---
default: patch
---

# Bump `rmcp` to 1.6 to address DNS rebinding advisory

Updates the `rmcp` Streamable HTTP server transport to 1.6.0, which patches [GHSA-89vp-x53w-74fx](https://github.com/modelcontextprotocol/rust-sdk/security/advisories/GHSA-89vp-x53w-74fx) (CVE-2026-42559). Host header validation is now performed inside `rmcp` itself, with a `tracing::warn!` event on each rejection so log-based alerting on DNS rebinding attempts continues to work; the server's existing `transport.streamable_http.host_validation` configuration is unchanged.
