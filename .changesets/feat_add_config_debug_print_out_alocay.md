### feat: adding a debug print out for the entire parsed configuration - @alocay PR #496

Adding a debug print out to display the entire parsed configuration at the start of the server.

Example output:
```
2025-11-18T16:12:29.253985Z  INFO Apollo MCP Server v1.2.0 // (c) Apollo Graph, Inc. // Licensed under MIT
2025-11-18T16:12:29.254074Z DEBUG Configuration: Config {
    cors: CorsConfig {
        enabled: true,
        origins: [],
        match_origins: [],
        allow_any_origin: true,
        allow_credentials: false,
        allow_methods: [
            "GET",
            "POST",
            "DELETE",
        ],
        allow_headers: [
            "content-type",
            "mcp-protocol-version",
            "mcp-session-id",
            "traceparent",
            "tracestate",
        ],
        ...
```
