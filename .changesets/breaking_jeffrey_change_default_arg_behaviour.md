### Deprecate -u,--uplink argument and use default collection - @Jephuff PR #154

`--uplink` and `-u` are deprecated and will act as an alias for `--uplink-manifest`. If a schema isn't provided, it will get fetched from uplink by default, and `--uplink-manifest` can be used to fetch the persisted queries from uplink.
The server will now default to the default MCP tools from operation collections.