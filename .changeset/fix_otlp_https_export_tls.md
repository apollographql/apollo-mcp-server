---
default: patch
---

# Fix OTLP HTTP exporter failing to connect to HTTPS endpoints

When the workspace upgraded from reqwest 0.12 to 0.13, Cargo feature unification stopped applying the workspace's TLS features to the reqwest 0.12 still used internally by opentelemetry-otlp. This left the OTLP HTTP exporter's reqwest client with no TLS backend, causing `"invalid URL, scheme is not http"` errors when exporting to any `https://` telemetry endpoint (e.g. Langfuse, New Relic). Adding the `reqwest-rustls` feature to opentelemetry-otlp restores TLS support for the internal reqwest 0.12 client.
