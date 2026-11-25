---
default: patch
---

# Hide server-wide operation tools from app tool lists

When a client connects to `/mcp?app=<name>`, `tools/list` now returns only the selected app's tools and, if enabled, the app-private `execute` tool. Previously, it also listed server-wide operation tools that the client could not call in app mode. Connect to `/mcp` without `?app=` to use those tools.
