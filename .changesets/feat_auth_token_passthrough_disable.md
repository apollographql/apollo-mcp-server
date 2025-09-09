### feat: Configuration for disabling authorization token passthrough - @swcollard PR #336

A new optional new MCP Server configuration parameter, `transport.auth.disable_auth_token_passthrough`, which is `false` by default, that when true, will no longer pass through validated Auth tokens to the GraphQL API.