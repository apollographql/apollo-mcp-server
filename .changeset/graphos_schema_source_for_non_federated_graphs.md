---
default: minor
---

# Add a GraphOS schema source for non-federated graphs

Uplink only distributes composed supergraphs, so non-federated (monograph) graphs had no live schema source and required a manually maintained local file. The new `schema.source: graphos` option fetches the latest published schema for the configured graph variant from the GraphOS Platform API, reusing the existing `graphos` credentials. The server polls the schema hash periodically, re-fetches the document only when it changes, and applies a republish without a restart.
