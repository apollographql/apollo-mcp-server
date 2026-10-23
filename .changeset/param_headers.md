---
default: minor
---

# Mirror operation variables into `Mcp-Param-*` headers

A new `overrides.param_headers` config key maps an operation's variables to SEP-2243 header names. The server annotates the matching tool parameters with `x-mcp-header`, so clients on protocol `2026-07-28` or later send each value in an `Mcp-Param-{name}` header as well as the body. A proxy can then route, meter, or authorize on the value without parsing the body.

The server rejects a `tools/call` whose `Mcp-Param-*` header is missing or doesn't match the body with HTTP 400 and JSON-RPC error `-32020`. Header names must be unique HTTP tokens, and annotated variables must be `String`, `ID`, `Int`, `Boolean`, enum, or a custom scalar with a `string`, `integer`, or `boolean` schema. The server doesn't load an operation whose entry breaks these rules.
