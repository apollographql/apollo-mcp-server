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

## [Unreleased]

### 🚀 Features
- Add an optional `--sse-address` argument to set the bind address of the MCP server. Defaults to 127.0.0.1. (#63)

### 🐛 Fixes
- Fixed PowerShell script (#55)
- Log to stdout, not stderr (#59)
- The `--directory` argument is now optional. When using the stdio transport, it is recommended to either set this option or use absolute paths for other arguments. (#64)

## [0.1.0] - 2025-05-15

### 🚀 Features
- Initial release of the Apollo MCP Server