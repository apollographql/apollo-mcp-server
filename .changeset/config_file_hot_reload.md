---
default: minor
---

# Add config file hot reloading

The MCP server now automatically detects changes to its YAML configuration file and reloads without requiring a restart. When a configuration change is detected, the server re-reads the file, applies the updated settings, and continues serving with the new configuration. If the updated config file contains errors, the server logs the issue and continues running with the previous configuration.
