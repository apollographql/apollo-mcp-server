---
default: minor
---

### Add outputSchema support - @DaleSeo PR #509

This PR implements support for the MCP specification's [outputSchema](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#output-schema) field on tools, which allows tools to declare the expected structure of their output. This helps LLMs better understand and reason about GraphQL response data.

This feature is opt-in to avoid additional token overhead. To enable it, add the following to your config:

```yaml
overrides:
  enable_output_schema: true
```
