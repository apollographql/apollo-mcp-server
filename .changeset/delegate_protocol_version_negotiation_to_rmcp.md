---
default: patch
---

# Delegate protocol version negotiation to rmcp

This server negotiated the `initialize` protocol version itself, echoing the client's requested version when supported and otherwise falling back to the newest revision it implements. rmcp 3.3.0 exposes that same rule through `ServerHandler::negotiate_initialize`, so the local copy has been removed in favor of the SDK's, along with the hand-filtered list of supported versions that `ProtocolVersion::known_up_to` now derives.

Negotiated versions are unchanged on every transport: a supported version is still echoed back, and anything newer or unrecognized still falls back to the newest revision this server implements.
