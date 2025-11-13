# [1.2.0] - 2025-11-13

## 🚀 Features

### Adding config option to specify metadata/header values for telemetry exporters - @alocay PR #460

Adding a `metadata` (for `grpc` protocol) and `headers` (for `http/protobuf` protocol) config option for the `telemetry.exporters.metrics.otlp` yaml config section.

```yaml
telemetry:
  exporters:
    metrics:
      otlp:
        endpoint: "http://127.0.0.1:4317"
        protocol: "grpc"
        metadata:
          the-key: some-value
    tracing:
      otlp:
        endpoint: "http://127.0.0.1:4317"
        protocol: "http/protobuf"
        headers:
          some-key: another-value
```

## 🐛 Fixes

### Allow using builtin names for custom tools - @dylan-apollo PR #481

Previously, the names of builtin tools were reserved even if the tool was disabled.
These names are now available for custom tools _if_ the matching builtin tool is disabled via config:
- `introspect`
- `search`
- `explorer`
- `execute`
- `validate`

### Improved performance of parallel tool calls - @dylan-apollo PR #475

Responsiveness of all tools is improved when many clients are connected.

