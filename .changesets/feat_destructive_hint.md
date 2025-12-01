### Set destructiveHint in tool annotations - @DaleSeo PR #510

This PR explicitly sets the `destructiveHint` annotation for MCP tools based on the GraphQL operation type. We now set `destructiveHint: false` for queries and `destructiveHint: true` for mutations to avoid relying on client-side spec compliance.

We currently only sets `readOnlyHint` based on whether the operation is a query and omits `destructiveHint`. Per [the MCP spec](https://modelcontextprotocol.io/legacy/concepts/tools#available-tool-annotations), `destructiveHint` defaults to true when omitted. The spec also says `destructiveHint` should be ignored when `readOnlyHint` is true, but OpenAI doesn't appear to implement this correctly.
