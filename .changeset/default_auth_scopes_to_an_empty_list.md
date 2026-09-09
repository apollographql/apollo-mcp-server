---
default: patch
---

# Default `transport.auth.scopes` to an empty list

`transport.auth.scopes` no longer has to be present in an auth configuration. Omitting it now means the same thing as an empty list, which is no global scope requirement. This matches its sibling list fields `audiences` and `issuers`, which already defaulted to empty, and it removes the `scopes: []` boilerplate that deployments running with `scope_mode: disabled` had to carry.

Previously an auth block without `scopes` failed at startup with `missing field 'scopes' for key "default.transport"`, an error that gave no hint that an empty list was an acceptable answer. The surrounding code already treated an empty list as valid, so only the deserialization requirement was out of step. `scopes` also drops out of the required list in the generated configuration JSON Schema.
