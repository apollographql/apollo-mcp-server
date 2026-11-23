---
default: patch
---

# Remove the startup warning about `transport.auth.resource` and `transport.path`

The server no longer warns at startup when `transport.auth.resource` doesn't seem to match `transport.path`. It can't see its public URL, so the warning fired on correct setups behind a proxy. Instead, it logs the configured resource at startup, and the auth docs explain how to match it to the URL clients use.
