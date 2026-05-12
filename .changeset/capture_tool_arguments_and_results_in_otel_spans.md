---
default: minor
---

# Capture tool call arguments and results in OpenTelemetry spans

Tool execution spans now include `apollo.mcp.tool_arguments` and `apollo.mcp.tool_result` attributes on the `call_tool` span, and `apollo.mcp.graphql_query` and `apollo.mcp.graphql_response` on the nested GraphQL `execute` span. This makes it possible to correlate traces in observability dashboards with the actual queries and data that triggered them. Arguments and results are serialized as JSON strings. Users who need to scrub sensitive data from spans should use the [OTel Collector `redaction` processor](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/processor/redactionprocessor) in their pipeline.
