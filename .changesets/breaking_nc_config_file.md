### Replace CLI flags with a configuration file - @nicholascioli PR #162

All command line arguments are now removed and replaced with equivalent configuration
options. The Apollo MCP server only accepts a single argument which is a path to a
configuration file. An empty file may be passed, as all options have sane defaults
that follow the previous argument defaults.

Below is a valid configuration file with all options filled out:

```yaml
custom_scalars: /path/to/custom/scalars
endpoint: http://127.0.0.1:4000
graphos:
  apollo_key: some.key
  apollo_graph_ref: example@graph
  apollo_registry_url: https://some.url
  apollo_uplink_endpoints:
    - http://uplink.endpoint.1
    - http://uplink.endpoint.2
headers:
  X-Some-Header: example-value
introspection:
  execute:
    enabled: true
  introspect:
    enabled: false
log_level: info
operations:
  type: local
  paths:
    - /path/to/operation.graphql
    - /path/to/other/operation.graphql
overrides:
  disable_type_description: false
  disable_schema_description: false
  enable_explorer: false
  mutation_mode: all
schema:
  type: local
  path: /path/to/schema.graphql
transport:
  type: sse
  address: 127.0.0.1
  port: 5000
```
