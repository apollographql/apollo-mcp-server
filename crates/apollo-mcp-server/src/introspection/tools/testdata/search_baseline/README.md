# AIR-399 — search-quality baseline

A fixed set of "search query → expected top-k results" fixtures captured against
**today's** apollo-mcp-server `search` tool over the offline catalog fixture in this
directory. This baseline is the parity gate the Discovery search migration (S2.5) must
match or beat, and the floor for later retrieval experiments.

## Files

- `catalog.graphql` — offline catalog fixture: a representative schema with
  service-prefixed domains (`Billing_`, `Inventory_`, `Shipping_`, `Support_`,
  `Accounts_`) plus unprefixed core types. The prefixes are deliberate: short
  natural-language queries against prefixed names are the known-hard case.
- `baseline.json` — the captured baseline. For each query: the ranked top-k root paths
  from the schema index (`top_paths`) and the type definitions returned by the MCP
  `search` tool (`result_types`). **Captured, never hand-edited.**

## Query coverage

- `unscoped-*` — plain domain terms with no service qualifier
- `scoped-*` — terms qualified by a service prefix, or exact prefixed names
- `hard-*` — short natural-language queries that must land on service-prefixed names

## CI gate

`cargo test -p apollo-mcp-server search_baseline` (part of the normal test suite) replays
every query against the current search implementation and fails on any deviation from
`baseline.json` — see `../../search_baseline.rs`.

The offline serve-smoke (`smoke.d/AIR-399.sh` at the repo root) additionally boots the
real server over streamable HTTP and asserts the MCP `search` tool reproduces
`result_types` end-to-end.

## Re-capturing

Only when the catalog fixture or search behavior changes intentionally:

```sh
cargo test -p apollo-mcp-server capture_search_baseline -- --ignored
```

Include the fixture diff in review: a diff here is a *search quality change*.
