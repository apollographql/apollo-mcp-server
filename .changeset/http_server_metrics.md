---
default: patch
---

# Export the standard HTTP server metrics again

`axum-otel-metrics` supplies the server's `http.server.*` metrics, and it takes
its meter from `opentelemetry::global`. It was pinned to a version built
against OpenTelemetry 0.30 while the server runs 0.32, and each major version
of that crate keeps its own global, so the layer recorded into a no-op provider
and nothing reached the configured exporter. Every metric the telemetry docs
attributed to that library was missing.

Upgrading `axum-otel-metrics` to 0.14.1 collapses the two OpenTelemetry
versions into one, and the metrics now reach the provider the server installs.
The documented names change with it: the duration histogram follows the current
conventions as `http.server.request.duration` rather than `http.server.duration`,
and `http.server.request.body.size` and `http.server.response.body.size` are
emitted alongside `http.server.active_requests`.
