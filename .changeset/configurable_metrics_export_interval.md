---
default: minor
---

# Configurable Metrics Export Interval

You can now customize how frequently metrics are exported to your observability backend using the new `export_interval` configuration option. The default remains 30 seconds.

```yaml
telemetry:
  exporters:
    metrics:
      export_interval: 1m # Supports human-readable values such as: 30s, 1m, 1h, 1d
```
