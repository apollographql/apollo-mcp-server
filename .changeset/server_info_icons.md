---
default: minor
---

# Advertise a server icon in `initialize`

Server operators can now configure `server_info.icons`, letting the MCP
`initialize` response advertise one or more icons that clients render
alongside the server's name. Each entry mirrors the fields of the MCP
[`Icon`](https://modelcontextprotocol.io/specification/2025-11-25/schema#icon)
object: `src`, `mime_type`, `sizes`, and `theme`. No icon is advertised
by default.
