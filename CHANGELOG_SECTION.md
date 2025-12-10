# [1.2.1] - 2025-11-18

## 🚀 Features

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

## 🐛 Fixes

### Fix fragment field validation in schema tree shaking - @DaleSeo PR #471

Fixed "field not found" errors that occurred when loading operations containing GraphQL fragments (inline fragments or fragment spreads) on union types or interfaces. The schema tree shaking algorithm now correctly handles fragments by evaluating them against their specific type conditions.

### Implement deduplication of operations - @DaleSeo PR #491

Fixed an issue where specifying both a directory and an explicit file path within that directory in the `operations.paths` configuration would create duplicate tools.
The server now automatically deduplicates operations based on their canonical file paths, ensuring that only one tool is created per unique operation file, regardless of how the paths are specified in the configuration.

### Index fields from interface implementing types - @DaleSeo PR #494

Fixed an issue where the search tool would not return results for fields that only exist on types implementing an interface. 
Now when a query returns an interface type, the search tool correctly indexes and searches all fields from implementing types, making implementation-specific fields discoverable even when accessed through interface types.

