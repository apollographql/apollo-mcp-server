---
default: patch
---

# Retry transient Platform API errors instead of failing startup

HTTP error responses from the GraphOS Platform API were parsed as JSON before their status was checked, so a 5xx or 429 surfaced as a body decode error that the retry policy classified as permanent. A transient GraphOS failure during the initial operation collection fetch therefore stopped the server at startup instead of retrying. The server now checks the response status first, so 5xx and 429 responses are correctly treated as transient and retried.
