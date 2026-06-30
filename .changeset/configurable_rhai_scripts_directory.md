---
default: patch
---

# Configure the Rhai scripts directory

Apollo MCP Server now supports configuring the directory used to load Rhai scripts with the top-level `rhai.scripts` option. The default remains `rhai`, preserving existing behavior, while deployments that mount scripts elsewhere can point startup loading and hot reload watching at that directory.
