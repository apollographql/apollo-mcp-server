---
default: patch
---

Replace shared MCP peer tracking with service-owned tool-change notification tasks. Slow clients no longer delay catalog updates or other clients, and a slow send no longer permanently disables notifications after five seconds. Legacy session teardown releases notification resources. Existing sessionless HTTP behavior and supported protocol versions are preserved. Catalog invalidations registered during initialization are retained until the initialized notification starts delivery, preventing missed updates while callbacks are scheduled.
