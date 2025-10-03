### feat: Add configuration option for metric temporality - @swcollard PR #413

Creates a new configuration option for telemetry to set the Metric temporality to either Cumulative (default) or Delta.

* Cumulative - The metric value will be the overall value since the start of the measurement.
* Delta - The metric will be the difference in the measurement since the last time it was reported.

Some observability  vendors require that one is used over the other so we want to support the configuration in the MCP Server.