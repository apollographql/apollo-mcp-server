### Add client metadata to GraphQL requests - @pubmodmatt PR #137

The MCP Server will now identify itself to Apollo Router through the `ApolloClientMetadata` extension. This allows traffic from MCP to be identified in the router, for example through telemetry.