### Add basic config file options to otel telemetry - @swcollard PR #330

Adds new Configuration options for setting up configuration beyond the standard OTEL environment variables needed before.

* Renames trace->telemetry
* Adds OTLP options for metrics and tracing to choose grpc or http upload protocols and setting the endpoints
* This configuration is all optional, so by default nothing will be logged