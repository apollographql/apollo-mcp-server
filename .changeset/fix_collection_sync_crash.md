---
default: patch
---

# Fix server crash on collection sync with invalid operations

A single operation with malformed variables JSON in a collection would crash the entire server. Invalid operations are now skipped with a warning, and the server continues serving with the remaining valid operations.
