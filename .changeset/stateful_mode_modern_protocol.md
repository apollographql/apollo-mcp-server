---
default: patch
---

# Don't reject an unauthenticated 2026-07-28 GET with 401 when `stateful_mode` is true

With `transport.auth` configured and `stateful_mode: true` (the default), an unauthenticated `GET` on the MCP endpoint that sends `Mcp-Protocol-Version: 2026-07-28` now returns 405 instead of 401. Protocol 2026-07-28 has no sessions, so the transport never serves that `GET`, whatever `stateful_mode` says. This is the same correction already made for `stateful_mode: false`.

`stateful_mode` only affects clients on protocol versions before 2026-07-28, and their behavior is unchanged: an unauthenticated `GET` without a version header, or with an older or unrecognized version, still returns 401.
