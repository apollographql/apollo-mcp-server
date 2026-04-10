---
default: minor
---

# Add MCP prompts support via Markdown files

Apollo MCP Server now supports [MCP prompts](https://modelcontextprotocol.io/docs/concepts/prompts). Prompts are reusable templates that guide AI models through multi-step workflows using your GraphQL tools.

Each prompt is a Markdown file in a `prompts/` directory with YAML frontmatter for metadata (name, description, arguments) and a template body with `{{argument}}` placeholders. The server loads prompts at startup and serves them via the `prompts/list` and `prompts/get` MCP methods. No configuration changes are needed — the server automatically detects the `prompts/` directory.
