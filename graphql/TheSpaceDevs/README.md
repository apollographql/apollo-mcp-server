# The Space Devs MCP Server

This folder contains an example usage of the Apollo MCP server for [The Space Devs](https://thespacedevs.com/) APIs, a set of APIs that exposes spaceflight information. We have a [hosted GraphQL endpoint](https://thespacedevs-production.up.railway.app/) that exposes The Space Devs Launch Library v2 REST APIs using Apollo Connectors. 

## Setup

To use this example, you must setup on of these three options to run the Apollo MCP server locally:

1. **_(Coming Soon)_** Use `rover dev` to run the Apollo MCP server - requires [installing `rover`](https://www.apollographql.com/docs/rover/getting-started)
2. Run the Docker image - requires having [Docker installed](https://docs.docker.com/engine/install/)
3. Build the `apollo-mcp-server` repo from source 

```bash
git clone https://github.com/apollographql/apollo-mcp-server
cd apollo-mcp-server
cargo build

# Built binaries will be located in ./target/debug/apollo-mcp-server
```

If you don't have an MCP client you plan on using right away, you can inspect the tools of the Apollo MCP server using the MCP Inspector:

```sh
npx @modelcontextprotocol/inspector
```

## Using STDIO and invoking Apollo MCP server with command

This option is typically used when you have built the source repository and use the binary outputs in the `target/build/*` folder.

There are operations located at `./operations/*.graphql` for you to use in your configuration. You can provide a set of operations in your MCP configuration along with the `--introspection` option that enables the LLM to generate a dynamic operation along with the ability to execute it. 

Here is an example configuration you can use _(Note: you must provide your fill path to the binary in the command. Make sure to replace the command with the path to where you cloned the repository)_:

```json
{
  "mcpServers": {
    "thespacedevs": {
      "command": "/Users/michaelwatson/Documents/GitHub/apollographql/apollo-mcp-server/target/debug/apollo-mcp-server",
      "args": [
        "graphql/TheSpaceDevs/config.yaml"
      ]
    }
  }
}
```

## Using Streamable HTTP with Apollo MCP server

There are operations located at `./operations/*.graphql` for you to use in your configuration. You can provide a set of operations in your MCP configuration that enables the LLM to generate a dynamic operation along with the ability to execute it. 

### Running with `rover dev`

```BASH
rover dev --supergraph-config supergraph.yaml --mcp config.yaml
```

### Running Apollo MCP server Docker image

1. Start up the MCP server locally

```bash
docker run \
  -it --rm \
  --name apollo-mcp-server \
  -p 8000:8000 \
  -v $PWD/graphql/TheSpaceDevs/config.yaml:/config.yaml \
  -v $PWD/graphql/TheSpaceDevs:/data \
  ghcr.io/apollographql/apollo-mcp-server:latest /config.yaml
```

2. Add the MCP port to your MCP Server configuration for the client application you are running. If you are running locally, the server link will be `http://127.0.0.1:8000/mcp`.

_Note: Claude Desktop currently doesn't support SSE_

```
{
  "mcpServers": {
    "thespacedevs": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://127.0.0.1:8000/mcp"
      ]
    }
  }
}
```

### Running binary built from source code

Here is an example configuration you can use _(Note: you must provide your fill path to the binary in the command. Make sure to replace the command with the path to where you cloned the repository)_:

```json
{
  "mcpServers": {
    "thespacedevs": {
      "command": "/Users/michaelwatson/Documents/GitHub/apollographql/apollo-mcp-server/target/debug/apollo-mcp-server",
      "args": [
        "graphql/TheSpaceDevs/config.yaml"
      ]
    }
  }
}
```
