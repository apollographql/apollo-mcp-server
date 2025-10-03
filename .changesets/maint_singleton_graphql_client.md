### chore: Only initialize a single HTTP client for graphql requests - @swcollard PR #412

Currently the MCP Server spins up a new HTTP client every time it wants to make a request to the downstream graphql endpoint. This change creates a static reqwest client that gets initialized using LazyLock and reused on each graphql request.

This change is based on the suggestion from the reqwest [documentation](https://docs.rs/reqwest/latest/reqwest/struct.Client.html)
> "The Client holds a connection pool internally, so it is advised that you create one and reuse it."