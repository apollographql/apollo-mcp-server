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
        "--directory",
        "/Users/michaelwatson/Documents/GitHub/apollographql/apollo-mcp-server/graphql/TheSpaceDevs",
        "--schema",
        "api.graphql",
        "--operations",
        "operations",
        "--endpoint",
        "https://thespacedevs-production.up.railway.app/",
        "--introspection"
      ]
    }
  }
}
```

## Using Server-Side-Events (SSE) with Apollo MCP server

There are operations located at `./operations/*.graphql` for you to use in your configuration. You can provide a set of operations in your MCP configuration along with the `--introspection` option that enables the LLM to generate a dynamic operation along with the ability to execute it. 

### Running SSE with `rover dev`

**_Coming soon_**

### Running Apollo MCP server Docker image

1. Start up the MCP server locally

```bash
docker run \
  -it --rm \
  --name apollo-mcp-server \
  -p 5000:5000 \
  -v $PWD/graphql/TheSpaceDevs:/data \
  ghcr.io/apollographql/apollo-mcp-server:latest \
  --sse-port 5000 \
  --schema api.graphql \
  --operations operations \
  --endpoint https://thespacedevs-production.up.railway.app/
```

2. Add the MCP SSE port to your MCP Server configuration for the client appliction you are running. If you are running locally, the server link will be `http://127.0.0.1:5000/sse`.

_Note: Claude Desktop currently doesn't support SSE_

```
{
  "mcpServers": {
    "thespacedevs": {
      "type": "sse",
      "url": "http://127.0.0.1:5000/sse>"
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
        "--directory",
        "/Users/michaelwatson/Documents/GitHub/apollographql/apollo-mcp-server/graphql/TheSpaceDevs",
        "--schema",
        "api.graphql",
        "--operations",
        "operations",
        "--endpoint",
        "https://thespacedevs-production.up.railway.app/",
        "--introspection"
      ]
    }
  }
}
```

## Using Persisted Queries - GraphOS Scale and Enterprise tiers only

You can configure the Apollo MCP server to use [Persisted Queries with GraphOS](https://www.apollographql.com/docs/graphos/routing/security/persisted-queries). In order to do this, you'll have to setup GraphOS and run a router instance configured to that persisted query list:

1. Create a new graph in [GraphOS](https://studio.apollographql.com/org)
2. Publish the `api.schema` to the graph you created, _you should see a modal pop up with the command information you need - make sure to save the API key as you'll use it again_

```
APOLLO_KEY=service:my-new-graph:V9_dIUACHIQh5VnhW21SXg \
  rover subgraph publish my-new-graph@current \
  --schema ./api.graphql \
  --name thespacedevs \
  --routing-url https://thespacedevs-production.up.railway.app/
```

3. Create a new [Persisted Queries List (PQL)](https://www.apollographql.com/docs/graphos/platform/security/persisted-queries#1-pql-creation-and-linking) for the newly created graph in GraphOS. Make sure to link it to your current variant.
4. Publish operations to PQL in GraphOS

```
rover persisted-queries publish \
  --graph-id my-new-graph \
  --list-id "PQL-ID" \
  --manifest ./persisted_queries/apollo.json
```

_Note: If you added new operations to the operations folder, you'll need to re-generate the persisted queries manifest. There is a VS Code Task you can use in the command palette "Tasks: Run Task" that runs the following command:_

```
npx @apollo/generate-persisted-query-manifest \
  generate-persisted-query-manifest \
  --config persisted_queries.config.json
```

5. In the command invoking or starting up your Apollo MCP Server with SSE, you'll need to export your `APOLLO_KEY` from the second step along with `APOLLO_GRAPH_REF=my-new-graph@current`.

```bash
export APOLLO_KEY=service:my-new-graph:V9_dIUACHIQh5VnhW21SXg
export APOLLO_GRAPH_REF=my-new-graph@current
```

6. Modify your Apollo MCP Server configuration to use the manifest option instead of operations.

```json
{
  "mcpServers": {
    "thespacedevs": {
      "command": "/Users/michaelwatson/Documents/GitHub/apollographql/apollo-mcp-server/target/debug/apollo-mcp-server",
      "args": [
        "--directory",
        "/Users/michaelwatson/Documents/GitHub/apollographql/mcp-apollo/graphql/TheSpaceDevs",
        "--schema",
        "api.graphql",
        "--manifest",
        "persisted_queries/apollo.json",
        "--endpoint",
        "https://thespacedevs-production.up.railway.app/",
        "--introspection"
      ]
    }
  }
}
```