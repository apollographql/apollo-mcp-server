# [0.8.0] - 2025-09-12

## 🚀 Features

### feat: Configuration for disabling authorization token passthrough - @swcollard PR #336

A new optional new MCP Server configuration parameter, `transport.auth.disable_auth_token_passthrough`, which is `false` by default, that when true, will no longer pass through validated Auth tokens to the GraphQL API.

## 🛠 Maintenance

### Configure Codecov with coverage targets - @DaleSeo PR #337

This PR adds `codecov.yml` to set up Codecov with specific coverage targets and quality standards. It helps define clear expectations for code quality. It also includes some documentation about code coverage in `CONTRIBUTING.md` and adds the Codecov badge to `README.md`.

### Implement Test Coverage Measurement and Reporting - @DaleSeo PR #335

This PR adds the bare minimum for code coverage reporting using [cargo-llvm-cov](https://crates.io/crates/cargo-llvm-cov) and integrates with [Codecov](https://www.codecov.io/). It adds a new `coverage` job to the CI workflow that generates and uploads coverage reporting in parallel with existing tests. The setup mirrors that of Router, except it uses `nextest` instead of the built-in test runner and CircleCI instead of GitHub Actions.

### chore: update RMCP dependency ([328](https://github.com/apollographql/apollo-mcp-server/issues/328))

Update the RMCP dependency to the latest version, pulling in newer specification changes.

### ci: Pin stable rust version ([Issue #287](https://github.com/apollographql/apollo-mcp-server/issues/287))

Pins the stable version of Rust to the current latest version to ensure backwards compatibility with future versions.

