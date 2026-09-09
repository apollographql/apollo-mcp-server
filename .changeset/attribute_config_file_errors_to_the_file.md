---
default: patch
---

# Report config file errors against the file instead of `APOLLO_MCP_` environment variables

A validation error in a YAML config file was reported as coming from the environment whenever an `APOLLO_MCP_<SECTION>__*` variable was set for the same top-level section. With `APOLLO_MCP_TRANSPORT__PORT` set, a config file missing `transport.auth.resource` failed with ``missing field `resource` for key "TRANSPORT" in `APOLLO_MCP_` environment variable(s)``, sending operators to look through their deployment environment rather than the file that actually had the problem. Figment tags a section assembled from several providers with whichever provider won precedence, and the environment always wins over the file, so it was credited for errors it did not cause.

Config errors are now attributed by extracting the config file on its own. Figment reports an error against a key path only as precise as the deserializer that failed, so for a section both sources filled in the path cannot say which one supplied the offending value, but a failure that survives without the environment belongs to the file. Errors that really do come from an `APOLLO_MCP_` variable, including one that overrides a value the file already set, still name the environment.

Errors from a config file also name its path now, for example ``missing field `resource` for key "default.transport" in config file 'router-config.yaml'``, where they previously said only "YAML source string".
