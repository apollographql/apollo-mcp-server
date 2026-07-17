---
default: patch
---

# Fall back to a global search when a `search` scope names no known service

The `search` tool's optional `scope` filter now validates the requested scope
against the services actually present in the schema (derived from operation-name
prefixes, e.g. `slack`, `ashby`). If the scope is not a known service, it is
dropped and the search runs across all services instead of silently returning no
results. A known scope is still honored even when it matches nothing for a given
query.

This removes wasted round-trips where an agent guesses a plausible-but-wrong
scope — for example `ats` when the service is `ashby`: previously the scoped
query returned nothing and the agent had to re-search unscoped. `SchemaSearch`
gains a `scopes()` method exposing the corpus's service set.
