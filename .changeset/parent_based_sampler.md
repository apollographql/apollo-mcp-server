---
default: minor
---

# Follow the caller's sampling decision for traces

Trace sampling is now parent-based by default. When a request carries a trace context, in a `traceparent` header or in MCP `_meta`, Apollo MCP Server follows the caller's sampling decision, and the configured `sampler` applies only to requests that start a new trace. This keeps traces complete across services and lets you force a trace while troubleshooting by sending a `traceparent` with the sampled flag (`01`), even with a low sampling ratio.

This changes behavior when callers send trace context: a sampled parent is now always recorded, and an unsampled one is never recorded, whatever `sampler` is set to. Set `telemetry.exporters.tracing.parent_based_sampler: false` to keep the previous behavior.
