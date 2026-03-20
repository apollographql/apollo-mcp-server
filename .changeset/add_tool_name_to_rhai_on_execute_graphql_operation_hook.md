---
default: minor
---

# Expose `tool_name` in the `on_execute_graphql_operation` Rhai hook

The `on_execute_graphql_operation` lifecycle hook now includes a read-only `tool_name` property on the context object. This lets you customize request behavior based on which MCP tool triggered the GraphQL operation.

```rhai
fn on_execute_graphql_operation(ctx) {
    if ctx.tool_name == "get_launch" {
        ctx.headers["x-priority"] = "high";
    }
}
```
