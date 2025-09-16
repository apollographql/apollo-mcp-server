### Update SDL handling in sdl_to_api_schema function - @lennyburdette PR #365

Loads supergraph schemas using a function that supports various features, including Apollo Connectors. When supergraph loading failed, it would load it as a standard GraphQL schema, which reveals Federation query planning directives in when using the `search` and `introspection` tools.
