---
default: patch
---

# Disable ANSI styling for non-terminal log output

Apollo MCP Server now emits ANSI-styled logs only when the output stream is connected to a terminal. Redirected stdout, stderr fallback, and configured log files use plain text, and a non-empty `NO_COLOR` environment variable disables ANSI styling for terminal output.
