### fix: Validate ExecutableDocument in validate tool - @swcollard PR #329

Contains fixes for https://github.com/apollographql/apollo-mcp-server/issues/327

The validate tool was parsing the operation passed in to it against the schema but it wasn't performing the validate function on the ExecutableDocument returned by the Parser. This led to cases where missing required arguments were not caught by the Tool.

This change also updates the input schema to the execute tool to make it more clear to the LLM that it needs to provide a valid JSON object