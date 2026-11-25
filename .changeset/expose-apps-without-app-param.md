---
default: minor
---

# Expose every app without the `app` parameter

A client that connects without the `app` parameter sees only server-wide tools, so using several apps took one connection per app. Set `apps.expose_without_app_param` to add every app's tools and UI resources to that connection. Each tool keeps its manifest name and opens its own app's UI.

```yaml
apps:
  expose_without_app_param: true
```

Startup fails if two apps have a tool with the same name.
