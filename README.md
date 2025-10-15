# DoControl MCP Server

This is a thin wrapper around [Apollo MCP Server](https://github.com/apollographql/apollo-mcp-server) configured specifically for DoControl's authentication flow.

## What is Apollo MCP Server?

Apollo MCP Server is a [Model Context Protocol](https://modelcontextprotocol.io/) server that exposes GraphQL operations as MCP tools. It provides a standard way for AI models to access and orchestrate GraphQL APIs.

For full documentation about Apollo MCP Server capabilities, see the [official documentation](https://www.apollographql.com/docs/apollo-mcp-server/).

## DoControl Authentication Flow

This wrapper handles DoControl's OAuth token refresh flow automatically:

### How It Works

1. **On-Demand Refresh**: Tokens are refreshed automatically before each request when needed
2. **Smart Detection**: Refreshes if token has less than 2 minutes remaining (out of 5-minute lifetime)
3. **Config Update**: Fresh tokens are written back to the config file's auth section
4. **Shared Headers**: Tokens are updated in both config file and in-memory headers atomically
5. **No Background Tasks**: Refresh happens synchronously when needed, not in background
6. **GraphQL Requests**: All operations use the current valid access token

This approach ensures:
- ✅ **No wasted refreshes** - Only refresh when token is actually needed
- ✅ **No startup delay** - Server starts instantly without initial token verification
- ✅ **Thread-safe** - Global token manager accessible from all request handlers
- ✅ **Reliable** - Synchronous refresh ensures token is valid before each request

**Note**: DoControl tokens have a 5-minute lifetime. The server refreshes tokens on-demand before executing requests to ensure they're always valid.

### Environment Variables

The server requires these environment variables:

```bash
# DoControl Token Refresh
DC_TOKEN_REFRESH_ENABLED="true"
DC_REFRESH_TOKEN="your-refresh-token-from-docontrol"
DC_REFRESH_URL="https://auth.prod.docontrol.io/refresh"
DC_GRAPHQL_ENDPOINT="https://apollo-gateway-v4-api.prod.docontrol.io/graphql"

# Apollo GraphOS API Key
DC_API_KEY="service:docontrol-api:your-apollo-key"

# Optional: Override the hardcoded graph ref (defaults to "docontrol-api@current")
# DC_GRAPH_REF="docontrol-api@current"
```

### Configuration File

Create a YAML configuration file (e.g., `config.yaml`):

```yaml
# GraphQL endpoint URL
endpoint: https://apollo-gateway-v4-api.prod.docontrol.io/graphql

# Transport configuration (stdio for MCP)
transport:
  type: stdio

# Authentication headers (automatically managed by token refresh)
headers:
  Authorization: Bearer <token-will-be-auto-refreshed>

# Apollo GraphOS integration
graphos:
  apollo-graph-ref: docontrol-api@current
  apollo-key: service:docontrol-api:your-apollo-key

# Mutation mode: "none" (read-only), "all" (full access)
allow-mutations: none

# Operation source: use introspection to discover operations
operations:
  - introspect

# Logging configuration
logging:
  level: error
  format: plain
  color: false

# Introspection tools configuration
introspection:
  execute:
    enabled: true    # Enable execute tool to run queries
  introspect:
    enabled: true    # Enable introspect tool for schema discovery
    minify: true     # Minify schema output
  search:
    enabled: true    # Enable search tool for finding types
    minify: true     # Minify search results
    index_memory_bytes: 50000000  # Memory limit for search index
    leaf_depth: 1    # Depth for leaf type expansion
  validate:
    enabled: true    # Enable validate tool for query validation
```

#### Configuration Options Explained

**Core Settings:**
- `endpoint`: The DoControl GraphQL API endpoint
- `transport.type`: `stdio` for MCP communication (required for MCP clients)

**Authentication:**
- `headers.Authorization`: Automatically updated by token refresh system
- The token in this section is managed by the server - it will be overwritten on startup and during refresh

**GraphOS Integration:**
- `apollo-graph-ref`: Your graph reference in Apollo Studio (e.g., `docontrol-api@current`)
- `apollo-key`: Your Apollo Studio API key for schema registry access

**Security:**
- `allow-mutations`: Set to `none` for read-only access, `all` to allow mutations

**Operations:**
- `introspect`: Use introspection to discover all queries and mutations automatically
- Alternative: `uplink` to use Apollo Studio operation collections

**Introspection Tools:**
The server provides 4 MCP tools when introspection is enabled:

1. **`execute`**: Run GraphQL queries and mutations
   - Validates operation syntax
   - Executes against the live endpoint
   - Returns JSON results

2. **`introspect`**: Explore the GraphQL schema
   - Get type information with hierarchy
   - Discover fields, arguments, and descriptions
   - Navigate relationships between types

3. **`search`**: Find types in the schema by name
   - Fuzzy search across all types
   - Returns matching type definitions
   - Useful for discovery

4. **`validate`**: Validate GraphQL operations before execution
   - Syntax checking
   - Schema validation
   - Helpful for debugging

**Note**: The `apollo_key` can reference environment variables using `${DC_API_KEY}` syntax.

**Note**: The `Authorization` header is automatically managed by the token refresh system. You don't need to manually update it.

## Quick Start Setup Guide

### Step 1: Install the Server

**Option A: Download from Releases**
```bash
# macOS (Apple Silicon)
curl -L https://github.com/docontrol-io/dc-mcp-server/releases/latest/download/dc-mcp-server-macos-aarch64.tar.gz | tar xz
chmod +x dc-mcp-server

# Linux
curl -L https://github.com/docontrol-io/dc-mcp-server/releases/latest/download/dc-mcp-server-linux-x86_64.tar.gz | tar xz
chmod +x dc-mcp-server

# Move to a permanent location
sudo mv dc-mcp-server /usr/local/bin/
```

**Option B: Build from Source**
```bash
git clone https://github.com/docontrol-io/dc-mcp-server.git
cd dc-mcp-server
cargo build --release
sudo cp target/release/dc-mcp-server /usr/local/bin/
```

### Step 2: Create Configuration File

Create a file named `docontrol-config.yaml`:

```yaml
endpoint: https://apollo-gateway-v4-api.prod.docontrol.io/graphql
transport:
  type: stdio
headers:
  Authorization: Bearer placeholder  # Will be auto-updated
graphos:
  apollo-graph-ref: docontrol-api@current
  apollo-key: service:docontrol-api:YOUR_APOLLO_KEY_HERE
allow-mutations: none
operations:
  - introspect
logging:
  level: error
  format: plain
  color: false
introspection:
  execute:
    enabled: true
  introspect:
    enabled: true
    minify: true
  search:
    enabled: true
    minify: true
  validate:
    enabled: true
```

**Replace `YOUR_APOLLO_KEY_HERE`** with your actual Apollo Studio API key.

### Step 3: Get Your Credentials

You'll need two secrets from DoControl:

1. **Refresh Token** (`DC_REFRESH_TOKEN`):
   - Obtain from DoControl OAuth authentication flow
   - This is a long-lived token used to get fresh access tokens
   - Keep this secret secure!

2. **Apollo API Key** (`DC_API_KEY`):
   - Format: `service:docontrol-api:xxxxx`
   - Used to access Apollo Studio for schema registry

### Step 4: Configure Your MCP Client

**For Cursor:**

Edit `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "dc-mcp-server": {
      "command": "/usr/local/bin/dc-mcp-server",
      "args": ["/absolute/path/to/docontrol-config.yaml"],
      "env": {
        "DC_TOKEN_REFRESH_ENABLED": "true",
        "DC_REFRESH_TOKEN": "YOUR_REFRESH_TOKEN_HERE",
        "DC_REFRESH_URL": "https://auth.prod.docontrol.io/refresh",
        "DC_GRAPHQL_ENDPOINT": "https://apollo-gateway-v4-api.prod.docontrol.io/graphql",
        "DC_API_KEY": "service:docontrol-api:YOUR_KEY_HERE",
        "RUST_LOG": "info",
        "RUSTLS_SYSTEM_CERT_ROOT": "1"
      }
    }
  }
}
```

**For Claude Desktop (macOS):**

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "dc-mcp-server": {
      "command": "/usr/local/bin/dc-mcp-server",
      "args": ["/absolute/path/to/docontrol-config.yaml"],
      "env": {
        "DC_TOKEN_REFRESH_ENABLED": "true",
        "DC_REFRESH_TOKEN": "YOUR_REFRESH_TOKEN_HERE",
        "DC_REFRESH_URL": "https://auth.prod.docontrol.io/refresh",
        "DC_GRAPHQL_ENDPOINT": "https://apollo-gateway-v4-api.prod.docontrol.io/graphql",
        "DC_API_KEY": "service:docontrol-api:YOUR_KEY_HERE",
        "RUST_LOG": "info",
        "RUSTLS_SYSTEM_CERT_ROOT": "1"
      }
    }
  }
}
```

**Important Notes:**
- Use **absolute paths** for both the command and config file
- Replace `YOUR_REFRESH_TOKEN_HERE` and `YOUR_KEY_HERE` with actual values
- The `RUSTLS_SYSTEM_CERT_ROOT=1` is required for SSL certificate validation

### Step 5: Restart Your MCP Client

- **Cursor**: Restart the application or reload the window
- **Claude Desktop**: Quit and restart the application

### Step 6: Verify Setup

In your MCP client, you should see 4 new tools available:
- ✅ `execute` - Run GraphQL queries
- ✅ `introspect` - Explore the schema
- ✅ `search` - Search for types
- ✅ `validate` - Validate queries

Try asking: "What is the company information?" or "Show me the company details"

## MCP Client Configuration Reference

### Using with MCP Inspector

For testing and debugging:

```bash
export DC_TOKEN_REFRESH_ENABLED="true"
export DC_REFRESH_TOKEN="your-refresh-token"
export DC_REFRESH_URL="https://auth.prod.docontrol.io/refresh"
export DC_GRAPHQL_ENDPOINT="https://apollo-gateway-v4-api.prod.docontrol.io/graphql"
export DC_GRAPH_REF="docontrol-api@current"
export DC_API_KEY="service:docontrol-api:your-key"

npx @modelcontextprotocol/inspector dc-mcp-server config.yaml
```

## How Introspection Works

The server automatically discovers all available GraphQL operations by introspecting the schema:

1. **Schema Discovery**: Introspects the GraphQL endpoint to discover all types and fields
2. **Tool Generation**: Each query and mutation becomes an MCP tool
3. **Dynamic**: Changes to the GraphQL schema are automatically reflected
4. **No Manual Configuration**: No need to maintain operation files

All queries and mutations from the DoControl GraphQL API are automatically available as tools to AI models.

## Installation

### From Release

Download the latest release for your platform from the [releases page](https://github.com/docontrol-io/dc-mcp-server/releases):
- **Linux**: `dc-mcp-server-linux-x86_64.tar.gz`
- **macOS**: `dc-mcp-server-macos-aarch64.tar.gz`
- **Windows**: `dc-mcp-server-windows-x86_64.tar.gz`

### From Source

```bash
cargo build --release --package dc-mcp-server
cp target/release/dc-mcp-server /usr/local/bin/
```

## Example Setup

1. **Create configuration file** (`config.yaml`):
```yaml
endpoint: "https://apollo-gateway-v4-api.prod.docontrol.io/graphql"
operations: introspect
introspection:
  query: true
  mutation: true
```

2. **Get your credentials**:
   - **Refresh Token**: From DoControl OAuth flow (secret)
   - **Apollo Graph Ref**: Your graph identifier, e.g., `docontrol-api@current` (internal)
   - **Apollo Key**: API key from Apollo Studio (secret)

3. **Configure your MCP client** with environment variables (see configuration examples above)

4. **Start your MCP client** - the server handles all authentication and operation discovery automatically!

The AI assistant will have access to all GraphQL queries and mutations from the DoControl API.

## Security Best Practices

**All credentials are secrets and should be protected:**

- ✅ **Never commit credentials** to version control
- ✅ **Use environment variables** instead of hardcoding in config files
- ✅ **Store tokens securely** - use secret management systems (e.g., 1Password, AWS Secrets Manager)
- ✅ **Rotate tokens regularly** - follow DoControl security best practices
- ✅ **Limit permissions** - use read-only tokens when possible

**Secrets to protect:**
- `DC_REFRESH_TOKEN` - DoControl OAuth refresh token
- `DC_API_KEY` - Apollo Studio API key
- Config files containing tokens

## How Token Refresh Works

The server uses an intelligent on-demand token refresh strategy:

### Startup
1. **Server Startup**: Reads `DC_REFRESH_TOKEN` from environment
2. **No Initial Refresh**: Server starts immediately without fetching tokens
3. **Global Token Manager**: TokenManager is initialized and stored globally
4. **Fast Startup**: No blocking network calls during initialization

### During Operation
1. **Before Each Request**: Token manager checks if current token is valid
2. **Token Expiry Check**: Refreshes if less than 2 minutes remaining (out of 5-minute lifetime)
3. **Synchronous Refresh**: If needed, refreshes token before executing the request
4. **Atomic Updates**: Updates both config file and in-memory headers together
5. **Error Handling**: If refresh fails, request proceeds with current token

### Token Lifetime
- **DoControl tokens expire after 5 minutes**
- **Refresh threshold: 2 minutes remaining** - ensures token won't expire during request
- First request after startup will always refresh (no initial token)
- Token is reused across multiple requests within the 3-minute window (5min - 2min threshold)
- Proactive refresh prevents mid-request token expiry

### Benefits
- **Efficient**: Tokens are reused across multiple requests
- **Reliable**: Token is always validated before use
- **Fast Startup**: Server is ready instantly
- **Thread-Safe**: Global static ensures safe concurrent access
- **No Background Tasks**: Simpler architecture, easier to debug

## Development

### Running Tests

```bash
cargo test --workspace
```

### Building

```bash
cargo build --release
```

### Debugging

Enable debug logging:
```bash
RUST_LOG=debug dc-mcp-server config.yaml
```

## Upstream

This project is based on [Apollo MCP Server](https://github.com/apollographql/apollo-mcp-server). For general MCP server features and documentation, refer to the upstream project.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
