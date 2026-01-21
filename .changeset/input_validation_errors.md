---
default: minor
---

### Return input validation errors as Tool Execution Errors - @DaleSeo PR #569

This change implements [SEP-1303](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1303) by modifying our error handling according to [the 2025-11-25 MCP spec](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#error-handling). The latest specification clarifies that input validation errors should be returned as Tool Execution Errors instead of Protocol Errors, allowing for model self-correction.