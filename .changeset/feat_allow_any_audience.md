---
default: minor
---

### Allow opting out of audience validation - @DaleSeo PR #535

Added an explicit `allow_any_audience` configuration option that follows the same pattern as CORS's `allow_any_origin`. When set to `true`, audience validation is skipped entirely.

```yaml
auth:
  servers:
    - https://auth.example.com

  # Validate specific audiences (default)
  audiences: ["my-api"]
  allow_any_audience: false

  # Or skip audience validation entirely
  audiences: []
  allow_any_audience: true## Changes
```
