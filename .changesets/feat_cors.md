### Add CORS support - @DaleSeo PR #362

This PR implements comprehensive CORS support for Apollo MCP Server to enable web-based MCP clients to connect without CORS errors. The implementation and configuration draw heavily from the Router's approach. Similar to other features like health checks and telemetry, CORS is supported only for the StreamableHttp transport, making it a top-level configuration.
