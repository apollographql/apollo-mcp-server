---
default: patch
---

# Substitute prompt arguments in a single pass

`prompts/get` now fills every `{{placeholder}}` in a prompt template in one
pass over the template. Previously each argument was substituted in turn over
the growing result, so a value that itself contained a placeholder was expanded
by a later argument. A client could use that to make the rendered prompt many
times larger than the request. Placeholder text inside an argument value is now
left as it is, and argument names that do not appear in the template are
ignored.
