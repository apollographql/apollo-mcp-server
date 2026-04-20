---
default: minor
---

# Add trace_id to log output for distributed trace correlation

Log lines emitted within an OpenTelemetry-traced span now include a `trace_id=<hex>` prefix, allowing operators to correlate logs with distributed traces in observability tools such as Jaeger and Grafana. Startup messages and other events outside a span are unaffected.
