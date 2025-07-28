# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

# [0.6.0] - 2025-07-14

## ❗ BREAKING ❗

### Replace CLI flags with a configuration file - @nicholascioli PR #162

All command line arguments are now removed and replaced with equivalent configuration
options. The Apollo MCP server only accepts a single argument which is a path to a
configuration file. An empty file may be passed, as all options have sane defaults
that follow the previous argument defaults.

All options can be overridden by environment variables. They are of the following
form:

- Prefixed by `APOLLO_MCP_`
- Suffixed by the config equivalent path, with `__` marking nested options.

E.g. The environment variable to change the config option `introspection.execute.enabled`
would be `APOLLO_MCP_INTROSPECTION__EXECUTE__ENABLED`.

Below is a valid configuration file with some options filled out:

```yaml
custom_scalars: /path/to/custom/scalars
endpoint: http://127.0.0.1:4000
graphos:
  apollo_key: some.key
  apollo_graph_ref: example@graph
headers:
  X-Some-Header: example-value
introspection:
  execute:
    enabled: true
  introspect:
    enabled: false
log_level: info
operations:
  source: local
  paths:
    - /path/to/operation.graphql
    - /path/to/other/operation.graphql
overrides:
  disable_type_description: false
  disable_schema_description: false
  enable_explorer: false
  mutation_mode: all
schema:
  source: local
  path: /path/to/schema.graphql
transport:
  type: streamable_http
  address: 127.0.0.1
  port: 5000
```

## 🚀 Features

### Add search tool - @pubmodmatt PR #171

A new experimental `search` tool has been added that allows the AI model to specify a set of terms to search for in the GraphQL schema. The top types matching that search are returned, as well as enough information to enable creation of GraphQL operations involving those types.



# [0.5.2] - 2025-07-10

## 🐛 Fixes

### Fix ServerInfo - @pubmodmatt PR #183

The server will now report the correct server name and version to clients, rather than the Rust MCP SDK name and version.



# [0.5.1] - 2025-07-08

## 🐛 Fixes

### Fix an issue with rmcp 0.2.x upgrade - @pubmodmatt PR #181

Fix an issue where the server was unresponsive to external events such as changes to operation collections.



# [0.5.0] - 2025-07-08

## ❗ BREAKING ❗

### Deprecate -u,--uplink argument and use default collection - @Jephuff PR #154

`--uplink` and `-u` are deprecated and will act as an alias for `--uplink-manifest`. If a schema isn't provided, it will get fetched from uplink by default, and `--uplink-manifest` can be used to fetch the persisted queries from uplink.
The server will now default to the default MCP tools from operation collections.

## 🚀 Features

### Add --version argument - @Jephuff PR #154

`apollo-mcp-server --version` will print the version of apollo-mcp-server currently installed

### Support operation variable comments as description overrides - @alocay PR #164

Operation comments for variables will now act as overrides for variable descriptions

### Include operation name with GraphQL requests - @DaleSeo PR #166

Include the operation name with GraphQL requests if it's available.

```diff
{
   "query":"query GetAlerts(: String!) { alerts(state: ) { severity description instruction } }",
   "variables":{
      "state":"CO"
   },
   "extensions":{
      "clientLibrary":{
         "name":"mcp",
         "version": ...
      }
   },
+  "operationName":"GetAlerts"
}
```

## 🐛 Fixes

### The execute tool handles invalid operation types - @DaleSeo PR #170

The execute tool returns an invalid parameters error when the operation type does not match the mutation mode.

### Skip unnamed operations and log a warning instead of crashing - @DaleSeo PR #173

Unnamed operations are now skipped with a warning instead of causing the server to crash

### Support retaining argument descriptions from schema for variables - @alocay PR #147

Use descriptions for arguments from schema when building descriptions for operation variables.

### Invalid operation should not crash the MCP Server - @DaleSeo PR #176

Gracefully handle and skip invalid GraphQL operations to prevent MCP server crashes during startup or runtime.

# [0.4.2] - 2025-06-24

## 🚀 Features

### Pass in --collection default to use default collection - @Jephuff PR #151

--collection default will use the configured default collection on the graph variant specified by the --apollo-graph-ref arg

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
