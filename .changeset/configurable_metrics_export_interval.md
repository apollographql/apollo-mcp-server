---
default: minor
---

# Configurable Metrics Export Interval

You can now customize how frequently metrics are exported to your observability backend using the new `export_interval` configuration option. The default remains 30 seconds.

```yaml
telemetry:
  exporters:
    metrics:
      export_interval: 1m # Supports: 30s, 1m, 2h, etc.
```
