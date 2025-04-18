# Apollo MCP Server

The MCP Server exposes a pre-defined set of GraphQL queries as MCP tools.

# Running the Example

The repo has an example schema in `graphql/weather/weather.graphql`, and an example set of operations in `graphql/weather/operations/*.graphql`.

First, build the repo with:

```sh
cargo build
```

Next, start the Apollo Router:

```sh
rover dev --supergraph-config ./graphql/weather/supergraph.yaml
```

Then, register the MCP Server with your AI agent.

For Claude Desktop, the configuration file is in the following location on MacOS:

```sh
~/Library/Application\ Support/Claude/claude_desktop_config.json
```

For Cursor, you can find the file by opening the MCP tab in Settings.

Add the following to this file, using the absolute path to this Git repo:

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

Restart your AI agent. You should now see the tools successfully registered. For example, in Claude Desktop, you should see a small hammer icon with the number of tools next to it.

You can now issue prompts related to weather forecasts and alerts, which will call out to the tools and invoke the GraphQL operations.

**Note** that due to current limitations of Apollo Connectors, the schema is using a hard-coded weather forecast link, so the forecast will be for a fixed location.

# Running Your Own Graph

You can easily run the server with your own GraphQL schema and operations in the AI agent configuration file:

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

In Claude Desktop, click the hammer icon to see the description generated for your tools.
