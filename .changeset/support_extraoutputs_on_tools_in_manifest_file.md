---
default: minor
---

# Support `extraOutputs` on tools in manifest file

Tools can now provide an `extraOutputs` argument. The content of this property is written as-is to the `extra` property under `structuredContent` on tool responses.
