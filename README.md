# Apollo MCP Server

The MCP Server exposes a pre-defined set of GraphQL queries as MCP tools.

# Running

The repo has an example schema in `.graphql/weather.graphql`, and an example set of operations in `.graphql/operations.json`.

First, build the repo with:

```sh
cargo build
```

Next, start the Apollo Router:

```sh
rover dev --supergraph-config ./graphql/supergraph.yaml
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
        "args": [ "<absolute path to repo>" ]
    }
  }
}
```

Restart your AI agent. You should now see the tools successfully registered. For example, in Claude Desktop, you should see a small hammer icon with the number of tools next to it.

You can now issue prompts related to weather forecasts and alerts, which will call out to the tools and invoke the GraphQL operations.

**Note** that due to current limitations of Apollo Connectors, the schema is using a hard-coded weather forecast link, so the forecast will always be the same.
