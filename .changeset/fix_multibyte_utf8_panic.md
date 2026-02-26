---
default: patch
---

# Fix panic with multibyte UTF-8 characters in GraphQL schemas

Update `apollo-compiler` to 1.31.1, which upgrades the `ariadne` error reporting library from 0.5.1 to 0.6.0. This fixes a panic when handling GraphQL schemas containing CJK text or emoji in field names or descriptions, where byte-offset span calculations could land inside a multibyte character boundary.
