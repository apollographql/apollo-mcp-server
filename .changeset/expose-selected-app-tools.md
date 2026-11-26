---
default: minor
---

# Expose selected app tools without the `app` parameter

A client that connects without the `app` parameter sees only server-wide operation tools, so using several apps took one connection per app. List app tools under `apps.expose_without_app_param` to add them to that connection. Each tool keeps its manifest name and opens its own app's UI.

```yaml
apps:
  expose_without_app_param:
    analytics: [usage_chart]
    records: [lookup]
```

Startup fails if two selected apps list the same tool name.
