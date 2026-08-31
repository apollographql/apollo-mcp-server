---
default: patch
---

# Cap protocol version negotiation on stateful transports

On stateful streamable HTTP sessions and stdio, rmcp's handshake re-negotiated the protocol version after this server's `initialize` handler ran, echoing back any version in rmcp's `KNOWN_VERSIONS` regardless of whether this server implements it. A stateful client requesting `2026-07-28`, a revision rmcp has a constant for but this server doesn't yet handle (SEP-2243 headers and `subscriptions/listen`), was told the server supports it and could send follow-up requests the server couldn't serve.

The rmcp upgrade to 3.2.0 adds a `ServerHandler::supported_protocol_versions` hook that rmcp's re-negotiation now consults on every transport. This server overrides it to the versions it actually implements, so the cap at the newest supported protocol version now holds consistently across stateless and stateful streamable HTTP and stdio.
