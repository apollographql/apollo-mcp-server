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