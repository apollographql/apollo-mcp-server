---
default: patch
---

# Stop ignoring `apollo_graph_ref` when inferring the operation source

When `operations.source` was not configured, Apollo MCP Server inferred the source by checking introspection settings first, which meant a configured `apollo_graph_ref` was silently ignored whenever any introspection tool (`execute`, `introspect`, `search`, `validate`) was also enabled. Now, when both `apollo_graph_ref` and `apollo_key` are available, the default GraphOS operation collection is loaded regardless of introspection settings. Users who have configured both will see operation tools alongside any enabled introspection tools.
