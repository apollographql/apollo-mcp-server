---
default: patch
---

### Add an optional MCP server conformance workflow

Manually run the official MCP conformance suite against the production server with
local GraphQL operations and prompts for the 2025-11-25 protocol revision.
Known fixture gaps are recorded per check so new protocol regressions and stale
expected failures fail the optional workflow.
Supplemental checks cover local MCP App resource content and errors, prompt
text, and currently passing unscored session lifecycle checks. Remaining
fixture and upstream coverage gaps are documented.
