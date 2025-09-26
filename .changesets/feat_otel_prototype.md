### Prototype OpenTelemetry Traces in MCP Server - @swcollard PR #274

Pulls in new crates and SDKs for prototyping instrumenting the Apollo MCP Server with Open Telemetry Traces.

* Adds new rust crates to support OTel
* Annotates excecute and call_tool functions with trace macro
* Adds Axum and Tower middleware's for OTel tracing
* Refactors Logging so that all the tracing_subscribers are set together in a single module.

