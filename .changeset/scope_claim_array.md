---
default: patch
---

# Accept an array-valued `scope` claim

The server now accepts a JWT whose `scope` claim is an array of strings, as well as a space-separated string. Duende IdentityServer emits `scope` as an array by default. Before, such a token failed to deserialize and the server returned HTTP 401 even when the issuer, audience, signature and expiry were all valid.
