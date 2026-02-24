---
default: minor
---

# Support custom tool descriptions for all operation sources

Users can now provide custom tool descriptions via the `overrides.descriptions` config, regardless of the operation source (manifest, uplink, collection, local files). This lets AI models better understand when and how to use each tool, without requiring changes to operation files or manifests.

```yaml
overrides:
  descriptions:
    GetAlerts: "Get active weather alerts for a US state"
    GetForecast: "Get a detailed weather forecast for a coordinate"
```
