# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Apollo MCP Server is a Model Context Protocol (MCP) server that exposes GraphQL operations as MCP tools for AI models. It provides a standard way for AI models to access and orchestrate APIs running with Apollo GraphOS.

## Build & Test Commands

```bash
# Build
cargo build
cargo build --release

# Run all tests
cargo test

# Run a single test
cargo test test_name

# Run tests for a specific crate
cargo test -p apollo-mcp-server

# Linting (CI enforces --deny warnings)
cargo clippy --all-targets -- --deny warnings

# Formatting
cargo fmt
cargo fmt --check

# Generate documentation
cargo doc --no-deps

# Code coverage (requires cargo-llvm-cov)
cargo llvm-cov --all-features --workspace --codecov --output-path codecov.json
```

## Workspace Structure

The project is a Rust workspace with three crates:

- **apollo-mcp-server** (`crates/apollo-mcp-server/`) - Main binary. MCP protocol handler, HTTP/WebSocket transport, GraphQL execution, auth, telemetry.
- **apollo-mcp-registry** (`crates/apollo-mcp-registry/`) - Schema and operations provider. Handles schema sources (Uplink, local files) and operation sources (Platform API collections, persisted queries, local files).
- **apollo-schema-index** (`crates/apollo-schema-index/`) - GraphQL schema indexing using Tantivy for full-text search.

## Key Architecture Patterns

**Server State Machine** (`apps/tools/states/`): The server transitions through states: Configuring → SchemaConfigured → OperationsConfigured → Running.

**MCP Apps** (`apps/`): Tool definitions (execute, introspect, search, validate), resources (schema, operations), and manifests.

**Operations Module** (`operations/`): Converts GraphQL operations into MCP tools, handles variable coercion and execution.

**Runtime Configuration** (`runtime/`): YAML config parsing with environment variable expansion (`${VAR}` syntax), telemetry setup, auth configuration.

## Code Quality Requirements

- **Clippy**: Strict lints enforced. Panics, unwraps, and expects are denied in library code.
- **Coverage**: 80% minimum on new/modified code (patch coverage).
- **Pre-commit**: Run `cargo test`, `cargo clippy`, `cargo fmt` before committing.
- **Testing**: Uses `rstest` for parameterized tests, `insta` for snapshot testing, `mockito` for HTTP mocking.

## Key Dependencies

- `rmcp 0.14` - MCP protocol implementation
- `apollo-compiler`, `apollo-federation` - GraphQL parsing and schema handling
- `axum 0.8` - HTTP server
- `tokio` - Async runtime
- `opentelemetry` - Observability

## Security Considerations

- Input validation on external data
- JWT authentication with JWKS support
- Host header validation for DNS rebinding protection
- Secrets are never logged (use `secrecy` crate)
