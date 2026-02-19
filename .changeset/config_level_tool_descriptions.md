---
default: minor
---

# Support custom tool descriptions for manifest operations

Users can now provide custom tool descriptions for operations loaded from persisted query manifests by adding a `descriptions` map to the `operations` config. This lets AI models better understand when and how to use each tool, without requiring changes to the standard manifest format.

```yaml
operations:
  source: manifest
  path: ./manifest.json
  descriptions:
    GetAlerts: "Get active weather alerts for a US state"
    GetForecast: "Get a detailed weather forecast for a coordinate"
```
