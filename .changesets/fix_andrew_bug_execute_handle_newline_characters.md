### Bug: `\n` characters in query breaking `execute` tool ([Issue #ISSUE_NUMBER](https://github.com/apollographql/apollo-mcp-server/issues/ISSUE_NUMBER))

Prior to this change, any `\n` present in a query body sent to `execute` would cause an error pointing to `\` as an unrecognized character.
