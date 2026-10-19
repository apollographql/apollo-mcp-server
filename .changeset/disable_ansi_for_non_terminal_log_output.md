---
default: minor
---

# Disable ANSI styling for non-terminal log output

Apollo MCP Server now emits ANSI-styled logs only when the output stream is connected to a terminal. Redirected output and configured log files use plain text, and a non-empty `NO_COLOR` environment variable disables ANSI styling for terminal output.

A non-empty `FORCE_COLOR` or `CLICOLOR_FORCE` environment variable (other than `"0"`) now enables ANSI styling even when the output isn't a terminal, useful for CI log viewers such as GitHub Actions that render ANSI from piped output. `NO_COLOR` takes precedence when both are set.

A new `logging.ansi` config key (`auto` | `always` | `never`, default `auto`) lets operators override terminal detection and all of the above environment variables explicitly.
