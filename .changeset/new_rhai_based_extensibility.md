---
default: minor
---

# New Rhai-based extensibility

With this release, we're introducing our first extensibility to the MCP Server. This utilizes [Rhai](https://rhai.rs/) as the script engine and allows you to hook into the MCP Server lifecycle.

For this release, we've introduced a single lifecycle hook:

```rhai
fn on_execute_graphql_operation(context){

}
```

From within this hook you can do a number of things including:

- Logging with `print`/`debug`
- Get/set the graphql endpoint with `context.endpoint`
- Get info about the incoming request with `context.incoming_request.headers["authorization"]`
- Get environment variables with `Env::get("MY_VARIABLE")`
- Sha256 hashes using `Sha256::digest("my string")`
- Get/set outgoing headers using `context.headers["x-my-header"] = "hello"`
- End requests early like `throw ${ code: ErrorCode::INVALID_REQUEST, message: "I ended!" }`
- JSON with `JSON::stringify(obj)` and `JSON::parse(json_string)`
- Regex operations like `Regex::is_match("hello world", "hello");` and `Regex::replace("foo bar foo", "foo", "baz");` and `Regex::matches("abc 123 def 456", "\\d+");`

We've got more hooks and functions that we're looking at introducing (E.g. `on_startup` hook, `Http::get()` method) but we'd love to hear feedback on what you'd like to see made available!
