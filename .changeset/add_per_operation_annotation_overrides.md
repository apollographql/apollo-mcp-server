---
default: minor
---

# Add per-operation MCP tool annotation overrides

Users can now configure MCP tool annotations per operation via `overrides.annotations` in the config file. Each entry maps an operation name to annotation hints (`title`, `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint`) that are merged with auto-detected defaults. Additionally, `idempotent_hint` is now auto-set to `true` for queries and `open_world_hint` is auto-set to `true` for all operations.
