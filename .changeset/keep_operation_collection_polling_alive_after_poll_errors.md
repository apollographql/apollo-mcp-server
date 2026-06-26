---
default: patch
---

# Keep operation collection polling alive after poll errors

Operation collection polling no longer stops permanently when a post-initial poll receives a GraphOS response error, such as an intermittent permission response or missing GraphQL data. Previously, those response errors were classified as non-transient and caused the poll loop to exit, so later edits to the Studio operation collection were not picked up until the server process restarted.

Apollo MCP Server now keeps serving the last known good operation set and retries on the next poll interval after any poll-loop failure. Initial collection load failures still retain their existing behavior: non-transient errors are reported as terminal startup/configuration failures before the server has a collection to serve.
