# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
## [x.x.x] - yyyy-mm-dd
### ❗ BREAKING ❗
### 🚀 Features
### 🐛 Fixes
### 🛠 Maintenance
### 📚 Documentation
-->
## [UNRELEASED]

### 🐛 Fixes
- Silence __typename used in operations errors (#79)

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