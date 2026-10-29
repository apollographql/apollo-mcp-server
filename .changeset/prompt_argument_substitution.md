---
default: patch
---

# Prompt argument values are no longer re-expanded

When a prompt argument value contains `{{placeholder}}` text, it now stays as
literal text in the rendered prompt instead of being filled in by a later
argument.
