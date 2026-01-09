### feat: add support for custom environment variable expansion - @gocamille PR #539

## Summary

This PR adds support for `${env.VAR_NAME}` syntax in configuration files, allowing users to reference custom environment variables without being limited to the `APOLLO_MCP_*` naming convention.

Closes #454.

## Changes

- `runtime/env_expansion.rs` (new module) - parser for variable expansion
- `runtime.rs` (modified) - integrates expansion into the `read_config()` function
- `config-file.mdx` - updated docs with syntax, escaping, and special characters handling

- **Note** The `APOLLO_MCP_*` environment variable(s) will still take precedence over expanded custom config values (no breaking change).