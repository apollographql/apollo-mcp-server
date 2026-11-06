---
default: patch
---

# Explain empty `search` results

The `search` tool now returns a text message that names the search terms when no type matches them, so clients can tell "nothing matched" apart from a dropped result. Calling `search` with an empty `terms` list now returns a tool error. Before, both cases returned a successful result with no content.
