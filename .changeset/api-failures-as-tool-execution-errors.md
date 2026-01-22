---
default: minor
---

### Return API failures as Tool Execution Errors - @DaleSeo PR #589

This PR aligns our error handling with  [the 2025-11-25 MCP spec](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#error-handling), which clarifies that API failures should be returned as Tool Execution Errors instead of Protocol Errors.

