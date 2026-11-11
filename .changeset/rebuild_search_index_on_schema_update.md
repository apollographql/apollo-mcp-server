---
default: patch
---

# Rebuild the `search` index when the schema changes

The `search` tool indexed the schema once at startup. When the schema changed
while the server was running (a local schema file edit, an Uplink or GraphOS
update), `introspect`, `validate` and `execute` saw the new schema, but
`search` kept answering from the old index: it missed added types and still
returned removed ones until the server restarted. The index is now rebuilt on
every schema update.
