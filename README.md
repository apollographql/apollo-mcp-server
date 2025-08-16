<div align="center">
<a href="https://www.apollographql.com/"><img src="https://raw.githubusercontent.com/apollographql/apollo-client-devtools/main/assets/apollo-wordmark.svg" height="100" alt="Apollo Client"></a>
</div>

# Apollo MCP Server

Apollo MCP Server is a [Model Context Protocol](https://modelcontextprotocol.io/) server that exposes GraphQL operations as MCP tools. It provides a standard way for AI models to access and orchestrate your APIs running with Apollo.

## Documentation

See [the documentation](https://www.apollographql.com/docs/apollo-mcp-server/) for full details. This README shows the basics of getting this MCP server running. More details are available on the documentation site.

## Installation

You can either build this server from source, if you have Rust installed on your workstation, or you can follow the [installation guide](https://www.apollographql.com/docs/apollo-mcp-server/install). To build from source, run `cargo build` from the root of this repository and the server will be built in the `target/debug` directory.

## Getting started

Follow the [quickstart tutorial](https://www.apollographql.com/docs/apollo-mcp-server/quickstart) to get started with this server.

## Usage

Full usage of Apollo MCP Server is documented on the [user guide](https://www.apollographql.com/docs/apollo-mcp-server/guides). There are a few items that are necessary for this server to function. Specifically, the following things must be configured:

1. A graph for the MCP server to sit in front of.
1. Definitions for the GraphQL operations that should be exposed as MCP tools.
1. A configuration file describing how the MCP server should run.
1. A connection to an MCP client, such as an LLM or [MCP inspector](https://modelcontextprotocol.io/legacy/tools/inspector).

These are all described on the user guide. Specific configuration options for the configuration file are documented in the [command reference](https://www.apollographql.com/docs/apollo-mcp-server/command-reference).

## Licensing

This project is licensed under the MIT License. See the [LICENSE](./LICENSE) file for the full license text.
