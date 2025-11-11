### Allow using builtin names for custom tools - @dylan-apollo PR #481

Previously, the names of builtin tools were reserved even if the tool was disabled.
These names are now available for custom tools _if_ the matching builtin tool is disabled via config:
- `introspect`
- `search`
- `explorer`
- `execute`
- `validate`
