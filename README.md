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

The MCP server also supports reading operations from an
[Apollo](https://www.apollographql.com/docs/graphos/platform/security/persisted-queries#manifest-format) formatted
persisted query manifest file through the use of the `--manifest` flag.

An example is included in `graphql/weather/persisted_queries`.

```sh
target/debug/mcp-apollo-server \
  --directory <absolute path to this git repo> \
  -s graphql/weather/api.graphql \
  --header "apollographql-client-name:my-web-app" \
  --manifest graphql/weather/persisted_queries/apollo.json
```

Note that when using persisted queries, if your queries are registered with a specific client name instead of `null`,
you will need to configure the MCP server to send the necessary header indicating the client name to the router. This
header is `apollographql-client-name` by default, but can be overridden in the router config by setting
`telemetry.apollo.client_name_header`. Note that in the example persisted query manifest file, the client name
is `my-web-app`.

This supports hot-reloading, so changes to the persisted query manifest file will be picked up by the MCP server
without restarting.

#### Uplink

The MCP server can also read persisted queries from Uplink using the `--uplink` option. This supports hot-reloading,
so it will pick up changes from GraphOS automatically, without restarting the MCP server.

You must set the `APOLLO_KEY` and `APOLLO_GRAPH_REF` environment variables to use Uplink. It is recommended to use
a contract variant of your graph, with a PQ list associated with that variant. That way, you control exactly what
persisted queries are available to the MCP server.

```sh
target/debug/mcp-apollo-server \
  --directory <absolute path to this git repo> \
  -s graphql/weather/api.graphql \
  --header "apollographql-client-name:my-web-app" \
  --uplink
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

# Licensing

Source code in this repository is covered by the Elastic License 2.0. The
default throughout the repository is a license under the Elastic License 2.0,
unless a file header or a license file in a subdirectory specifies another
license. [See the LICENSE](./LICENSE) for the full license text.