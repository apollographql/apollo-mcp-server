---
default: patch
---

### Fix transient error handling in collection polling loop - @DaleSeo PR #576

The MCP server currently restarts when it encounters network errors while polling operation collections from GraphOS. This causes unnecessary server restarts during temporary connectivity issues.

This PR applies the same transient error handling pattern to both polling loops (`collection_id_stream` and `default_collection_stream`). Transient errors are now logged as warnings and the server retries on the next poll interval (30 seconds), while permanent errors like permission denied or not found still trigger a restart.