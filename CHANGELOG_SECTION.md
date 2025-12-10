# [1.3.0] - 2025-12-10

## 🚀 Features

### Set destructiveHint in tool annotations - @DaleSeo PR #510

This PR explicitly sets the `destructiveHint` annotation for MCP tools based on the GraphQL operation type. We now set `destructiveHint: false` for queries and `destructiveHint: true` for mutations to avoid relying on client-side spec compliance.

We currently only sets `readOnlyHint` based on whether the operation is a query and omits `destructiveHint`. Per [the MCP spec](https://modelcontextprotocol.io/legacy/concepts/tools#available-tool-annotations), `destructiveHint` defaults to true when omitted. The spec also says `destructiveHint` should be ignored when `readOnlyHint` is true, but OpenAI doesn't appear to implement this correctly.

## 🐛 Fixes

### Fix broken non nullable return types in minified schema - @esilverm PR #514

Fix handling of minified return types to support non-null lists and other non-null cases to improve operation construction accuracy.

## 🛠 Maintenance

### Clean up sanitize module - @DaleSeo PR #503

Just cleaning up unused code

### Abstract away operation execution logic from the server's running state - @DaleSeo PR #517

I abstracted the operation execution logic from the server's running state, following the pattern used in apps. This change helped me write tests and identify a subtle bug where the execute tool wasn't propagating the OTel context.

