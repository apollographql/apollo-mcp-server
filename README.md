<div align="center">
<a href="https://www.apollographql.com/"><img src="https://raw.githubusercontent.com/apollographql/apollo-client-devtools/main/assets/apollo-wordmark.svg" height="100" alt="Apollo Client"></a>
</div>

![version](https://img.shields.io/github/v/release/apollographql/apollo-mcp-server)
![ci workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/ci.yml)
![release binaries workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-bins.yml?label=release%20binaries)
![release container workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-container.yml?label=release%20container)
![license](https://img.shields.io/github/license/apollographql/apollo-mcp-server)
[![codecov](https://codecov.io/github/apollographql/apollo-mcp-server/graph/badge.svg?token=6NHuvZQ8ak)](https://codecov.io/github/apollographql/apollo-mcp-server)

# Apollo MCP Server

Apollo MCP Server is a [Model Context Protocol](https://modelcontextprotocol.io/) server that exposes GraphQL operations as MCP tools. It provides a standard way for AI models to access and orchestrate your APIs running with Apollo.

## Documentation

See [the documentation](https://www.apollographql.com/docs/apollo-mcp-server/) for full details. This README shows the basics of getting this MCP server running. More details are available on the documentation site.

## Installation

You can either build this server from source, if you have Rust installed on your workstation, or you can follow the [installation guide](https://www.apollographql.com/docs/apollo-mcp-server/run). To build from source, run `cargo build` from the root of this repository and the server will be built in the `target/debug` directory.

## Getting started

Follow the [quickstart tutorial](https://www.apollographql.com/docs/apollo-mcp-server/quickstart) to get started with this server.

## Usage

Full usage of Apollo MCP Server is documented on the [user guide](https://www.apollographql.com/docs/apollo-mcp-server/run). There are a few items that are necessary for this server to function. Specifically, the following things must be configured:

1. A graph for the MCP server to sit in front of.
2. Definitions for the GraphQL operations that should be exposed as MCP tools.
3. A configuration file describing how the MCP server should run.
4. A connection to an MCP client, such as an LLM or [MCP inspector](https://modelcontextprotocol.io/legacy/tools/inspector).

These are all described on the user guide. Specific configuration options for the configuration file are documented in the [config file reference](https://www.apollographql.com/docs/apollo-mcp-server/config-file).

## Contributions

Checkout the [contributor guidelines](https://github.com/apollographql/apollo-mcp-server/blob/main/CONTRIBUTING.md) for more information.

## Licensing

This project is licensed under the MIT License. See the [LICENSE](./LICENSE) file for the full license text.

# Security

Refer to our [security policy](https://github.com/apollographql/.github/blob/main/SECURITY.md).

> [!IMPORTANT]  
> **Do not open up a GitHub issue if a found bug is a security vulnerability**, and instead to refer to our [security policy](https://github.com/apollographql/.github/blob/main/SECURITY.md).
