### Remove --uplink argument - @Jephuff PR #154

The `--uplink` argument was removed. The schema will now be fetched from uplink by default if no schema is passed in.
To use the persisted queries from uplink, you can now pass in `--uplink-manifest`.
The server will now default to the default MCP tools from operation collections.