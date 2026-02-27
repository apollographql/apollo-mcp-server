---
default: patch
---

# Allow missing `aud` claim in access tokens for AWS Cognito compatibility

AWS Cognito access tokens omit the `aud` claim entirely unless resource binding with managed login is used. Previously, this caused JWT validation to fail with "missing field `aud`" even when `allow_any_audience: true` was configured. The `aud` claim is now optional during deserialization, and tokens without it are accepted when `allow_any_audience` is enabled. When `allow_any_audience` is false, tokens missing `aud` are still explicitly rejected.
