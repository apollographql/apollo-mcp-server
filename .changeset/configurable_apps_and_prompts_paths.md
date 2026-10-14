---
default: minor
---

# Configure where apps and prompts are loaded from

The directories the server reads MCP Apps and prompts from can now be set in the config file, matching the other file inputs such as `schema.path`, `operations.paths`, and `rhai.scripts`:

```yaml
apps:
  path: path/to/apps
prompts:
  path: path/to/prompts
```

Both accept absolute paths or paths relative to the process working directory, and can also be set with the `APOLLO_MCP_APPS__PATH` and `APOLLO_MCP_PROMPTS__PATH` environment variables. They default to `apps` and `prompts`, so existing deployments are unaffected.
