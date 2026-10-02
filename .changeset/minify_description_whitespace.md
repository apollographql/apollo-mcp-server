---
default: patch
---

# Keep word spacing in minified descriptions

Minified `introspect` and `search` results previously removed all whitespace from type, field, argument, and input field descriptions and `@deprecated` reasons, merging words together. Each whitespace run now collapses to a single space and leading and trailing whitespace is trimmed, so descriptions stay compact and readable.
