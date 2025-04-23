# Apollo MCP Server

The MCP Server exposes a pre-defined set of GraphQL queries as MCP tools.

# Running the Example

The repo has an example schema in `graphql/weather/weather.graphql`, and an example set of operations in `graphql/weather/operations/*.graphql`.

First, build the repo with:

```sh
cargo build
```

Next, run the graph in the Apollo Router:

```sh
rover dev --supergraph-config ./graphql/weather/supergraph.yaml
```

## MCP Inspector

You can run the MCP Server with [MCP Inspector](https://modelcontextprotocol.io/docs/tools/inspector).

### Stdio Transport

You can run the MCP inspector with the stdio transport as follows:

```sh
npx @modelcontextprotocol/inspector \
  target/debug/mcp-apollo-server \
  --directory <absolute path to this git repo> \
  -s graphql/weather/api.graphql \
  -o graphql/weather/operations/forecast.graphql graphql/weather/operations/alerts.graphql graphql/weather/operations/all.graphql
```

Press "Connect" in the MCP Inspector and "List Tools" to see the list of available tools.

### HTTP+SSE Transport

To use the SSE transport with MCP Inspector, first start the MCP server in SEE mode:

```sh
target/debug/mcp-apollo-server \
  --directory <absolute path to this git repo> \
  --sse-port 5000 -s graphql/weather/api.graphql \
  -o graphql/weather/operations/forecast.graphql graphql/weather/operations/alerts.graphql graphql/weather/operations/all.graphql
```

Now start the MCP Inspector:

```sh
npx @modelcontextprotocol/inspector
```

Set the transport to SSE in the inspector and the URL to `http://localhost:5000/sse`, then press "Connect" in MCP Inspector.

## MCP Client

You can use the MCP Server with your favorite MCP client.

### Client Configuration

For Claude Desktop, the configuration file is in the following location on MacOS:

```sh
~/Library/Application\ Support/Claude/claude_desktop_config.json
```

For Cursor, you can find the file by opening the MCP tab in Settings.

#### Stdio Transport

To use the stdio transport, add the following to the MCP configuration file for you client, using the absolute path to this Git repo:

```json
{
  "mcpServers": {
    "weather": {
        "command": "<absolute path to repo>/target/debug/mcp-apollo-server",
        "args": [
            "--directory",
            "<absolute path to repo>",
            "--schema",
            "graphql/weather/weather.graphql",
            "--operations",
            "graphql/weather/operations/forecast.graphql",
            "graphql/weather/operations/alerts.graphql",
            "graphql/weather/operations/all.graphql"
        ]
    }
  }
}
```

#### HTTP+SEE Transport

To use the HTTP+SSE transport, first start the MCP server as described above for MCP Inspector.

For Claude Desktop, you can use `mcp-remote` to give Claude access to the MCP Server over SSE:

```json
{
  "mcpServers": {
    "weather": {
        "command": "npx",
        "args": [
            "mcp-remote",
            "http://127.0.0.1:5000/sse"
        ]
    }
  }
}
```

For Cursor, you can directly specify the SSE URL:

```json
{
  "mcpServers": {
    "weather": {
        "url": "http://127.0.0.1:5000/sse"
    }
  }
}
```

### Usage

Restart your AI agent. You should now see the tools successfully registered. For example, in Claude Desktop, you should see a small hammer icon with the number of tools next to it.

You can now issue prompts related to weather forecasts and alerts, which will call out to the tools and invoke the GraphQL operations.

**Note** that due to current limitations of Apollo Connectors, the schema is using a hard-coded weather forecast link, so the forecast will be for a fixed location.

#### Persisted Queries Manifests

The MCP server also supports reading operations from either an [Apollo]()- or [Relay]()-formatted persisted query manifest through
the use of the `--manifest` and `--manifest-format` flags. An example of each is included in `graphql/weather/persisted_queries`.

```sh
# For apollo persisted query manifests
target/debug/mcp-apollo-server \
  --directory <absolute path to this git repo> \
  -s graphql/weather/api.graphql \
  --manifest graphql/weather/persisted_queries/apollo.json --manifest-format apollo

# Or for relay persisted query manifests
target/debug/mcp-apollo-server \
  --directory <absolute path to this git repo> \
  -s graphql/weather/api.graphql \
  --manifest graphql/weather/persisted_queries/relay.json --manifest-format relay
```

# Running Your Own Graph

You can easily run the server with your own GraphQL schema and operations. For example with the stdio transport:

```json
{
  "mcpServers": {
    "<name for your server>": {
        "command": "<absolute path to repo>/target/debug/mcp-apollo-server",
        "args": [
            "--directory",
            "<absolute path to the directory containing your schema and operations file>",
            "--schema",
            "<relative path from the directory specified above to the schema>",
            "--operations",
            "<relative path from the directory specified above to the operation files>",
            "--endpoint",
            "<your GraphQL endpoint>"
        ]
    }
  }
}
```

The operation files are just `.graphql` files, with each file containing a single operation. Make sure to give your operations meaningful names, and document your schema as much as possible.

Run your schema in Apollo Router at the endpoint given in your configuration file.

Use MCP Inspector, or in Claude Desktop, click the hammer icon to see the description generated for your tools.

# Introspection

You can optionally enable support for allowing the AI model to introspect the schema and formulate its own queries. It is recommended that this only be done with a Contract variant schema, so you can control what parts of your schema are exposed to the model.

To enable this mode, add `--introspect` to the MCP server command line.

Two new tools will be exposed by the server:

* `schema` - returns the GraphQL schema
* `execute` - executes an operation on the GraphQL endpoint

The MCP client can then use these tools to provide schema information to the model, and allow the model to execute GraphQL operations based on that schema.
