---
default: patch
---

# Fix misleading error when APOLLO_KEY is missing

When `APOLLO_KEY` was not set, the server incorrectly reported "Missing environment variable: APOLLO_GRAPH_REF" instead of `APOLLO_KEY`. This was a copy-paste bug in `GraphOSConfig::key()` that referenced the wrong constant in its error path.
