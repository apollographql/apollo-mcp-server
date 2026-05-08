---
default: minor
---

# Expose `trace_id` to the `on_execute_graphql_operation` Rhai hook

The `on_execute_graphql_operation` Rhai hook now exposes a read-only `ctx.trace_id` property, allowing scripts to access the current OpenTelemetry trace ID for custom structured logging. The value is a 32-character lowercase hex string when an OpenTelemetry trace context is active and an empty string otherwise, matching the format already used for the `trace_id=<hex>` prefix on server log lines. This makes it possible to emit log lines from Rhai with `trace_id` as a discrete field that log aggregators (Splunk, ELK, etc.) can index for correlation with distributed traces.
