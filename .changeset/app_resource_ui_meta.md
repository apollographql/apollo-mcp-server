---
default: patch
---

# Omit empty `_meta.ui` from MCP App resources

When an app manifest sets no CSP or widget settings, `resources/read` no longer returns `"_meta": {"ui": null}`. MCP Apps requires `_meta.ui` to be an object when present, so hosts that validate resource metadata could reject the app.
