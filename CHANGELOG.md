# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

# [0.4.1] - 2025-06-20

## 🐛 Fixes

### Fix tool update on every poll - @Jephuff PR #146

Only update the tool list if an operation was removed, changed, or added.



# [0.4.0] - 2025-06-17

## 🚀 Features

### Add `--collection <COLLECTION_ID>` as another option for operation source - @Jephuff PR #118

Use operation collections as the source of operations for your MCP server. The server will watch for changes and automatically update when you change your operation collection.

### Allow overriding registry endpoints - @Jephuff PR #134

Set APOLLO_UPLINK_ENDPOINTS and APOLLO_REGISTRY_URL to override the endpoints for fetching schemas and operations

### Add client metadata to GraphQL requests - @pubmodmatt PR #137

The MCP Server will now identify itself to Apollo Router through the `ApolloClientMetadata` extension. This allows traffic from MCP to be identified in the router, for example through telemetry.

### Update license to MIT - @kbychu PR #122

The Apollo MCP Server is now licensed under MIT instead of ELv2

## 🐛 Fixes

### Fix GetAstronautsCurrentlyInSpace query - @pubmodmatt PR #114

The `GetAstronautsCurrentlyInSpace` in the Quickstart documentation was not working.

### Change explorer tool to return URL - @pubmodmatt PR #123

The explorer tool previously opened the GraphQL query directly in the user's browser. Although convenient, this would only work if the MCP Server was hosted on the end user's machine, not remotely. It will now return the URL instead.

### Fix bug in operation directory watching - @pubmodmatt PR #135

Operation directory watching would not trigger an update of operations in some cases.

### fix: handle headers with colons in value - @DaleSeo PR #128

The MCP server won't crash when a header's value contains colons.

## 🛠 Maintenance

### Automate changesets and changelog - @pubmodmatt PR #107

Contributors can now generate a changeset file automatically with:

```console
cargo xtask changeset create
```

This will generate a file in the `.changesets` directory, which can be added to the pull request.

## [0.3.0] - 2025-05-29

### 🚀 Features

- Implement the Streamable HTTP transport. Enable with `--http-port` and/or `--http-address`. (#98)
- Include both the type description and field description in input schema (#100)
- Hide String, ID, Int, Float, and Boolean descriptions in input schema (#100)
- Set the `readOnlyHint` tool annotation for tools based on GraphQL query operations (#103)

### 🐛 Fixes

- Fix error with recursive input types (#100)

## [0.2.1] - 2025-05-27

### 🐛 Fixes

- Reduce the log level of many messages emitted by the server so INFO is less verbose, and add a `--log` option to specify the log level used by the MCP Server (default is INFO) (#82)
- Ignore mutations and subscriptions rather than erroring out (#91)
- Silence \_\_typename used in operations errors (#79)
- Fix issues with the `introspect` tool. (#83)
  - The tool was not working when there were top-level subscription in the schema
  - Argument types were not being resolved correctly
- Improvements to operation loading (#80)
  - When specifying multiple operation paths, all paths were reloaded when any one changed
  - Many redundant events were sent on startup, causing verbose logging about loaded operations
  - Better error handling for missing, invalid, or empty operation files
- The `execute` tool did not handle variables correctly (#77 and #93)
- Cycles in schema type definitions would lead to stack overflow (#74)

## [0.2.0] - 2025-05-21

### 🚀 Features

- The `--operations` argument now supports hot reloading and directory paths. If a directory is specified, all .graphql files in the directory will be loaded as operations. The running server will update when files are added to or removed from the directory. (#69)
- Add an optional `--sse-address` argument to set the bind address of the MCP server. Defaults to 127.0.0.1. (#63)

### 🐛 Fixes

- Fixed PowerShell script (#55)
- Log to stdout, not stderr (#59)
- The `--directory` argument is now optional. When using the stdio transport, it is recommended to either set this option or use absolute paths for other arguments. (#64)

### 📚 Documentation

- Fix and simplify the example `rover dev --mcp` commands

## [0.1.0] - 2025-05-15

### 🚀 Features

- Initial release of the Apollo MCP Server
