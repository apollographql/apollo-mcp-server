---
default: patch
---

# Warn when sensitive headers are forwarded

The server now logs a warning when sensitive credential headers such as `Authorization`, `Cookie`, `Proxy-Authorization`, or `X-Api-Key` are forwarded to the upstream GraphQL API. The warning is emitted when the header is actually present in an incoming request.
