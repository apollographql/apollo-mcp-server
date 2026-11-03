---
default: patch
---

# Default the OTLP exporter `protocol` to `grpc`

The `protocol` of the trace and metrics OTLP exporters was documented as defaulting to `grpc`, but the server refused to start without it. It's now optional and defaults to `grpc`, and the configuration JSON schema no longer requires it. The `endpoint` of both exporters is required, and the docs no longer list a default for it.
