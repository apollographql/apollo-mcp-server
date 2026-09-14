---
default: patch
---

# Fix compilation after the server icon merge

Fix compilation errors introduced by the server icon merge after the service refactor.
The service reads icons from the application metadata, and the icon tests use the service handler.
