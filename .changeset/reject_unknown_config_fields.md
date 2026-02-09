---
default: minor
---

# Validate configuration at startup

Validate configuration at startup. Invalid or misplaced configuration options (e.g., `auth` at the top level instead of nested under `transport`) now cause the server to fail with a clear error message listing the valid options, instead of being silently ignored.
