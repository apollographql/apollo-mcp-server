---
default: minor
---

# Publish to crates.io

Apollo MCP Server is now published to crates.io, so it can be built from source through a crates.io mirror:

```sh
cargo install --locked apollo-mcp-server
```

`--locked` builds with the same dependency versions that were tested for the release.

The supporting crates (`apollo-mcp-registry`, `apollo-mcp-rhai`, and `apollo-schema-index`) are published alongside it as internal dependencies. They are not a stable public API and may change in any release.
