# End-to-end telemetry

This is an optional, manually triggered check that the server's traces,
metrics, and logs work with generic OpenTelemetry tooling, with no
server-specific mapping. In GitHub Actions, select **Telemetry end to end**,
choose **Run workflow**, and select the branch to test. Run it after changing
spans, metrics, sampling, or the OpenTelemetry crates. Like the conformance
check, it doesn't run on pull requests.

To run it locally from the repository root:

```bash
bash e2e/telemetry/run.sh
```

It needs Docker with Compose, Node 22, and the repository's Rust toolchain, and
ports 4200, 4201, 4317, 9090, and 16686 on loopback. Pass `--keep` to leave the
backends running afterward, so you can browse the traces in Jaeger at
http://127.0.0.1:16686 and the metrics in Prometheus at http://127.0.0.1:9090.

## What it runs

`compose.yaml` starts an OpenTelemetry Collector that forwards traces to Jaeger
and exposes metrics for Prometheus to scrape. The script builds the server and
starts it with `config.yaml`, which exports OTLP to the Collector, writes JSON
logs, and samples new traces at a ratio of 0.5. A local GraphQL fixture answers
the server's one tool. The script sends stateless MCP requests with chosen trace
contexts, then reads the results back from the Jaeger and Prometheus APIs.

The sampler's ratio keeps or drops a trace based on the low 64 bits of its ID,
so the script picks trace IDs whose fate under the ratio is known. That way a
trace that appears or doesn't can only be the caller's sampled flag at work, or
the ratio when `parent_based_sampler` is off.

## What it checks

- A `traceparent` header parents the HTTP request span, the MCP handler span
  nests under it, and the trace continues to the GraphQL endpoint.
- A `_meta.traceparent` parents the handler span, and the handler span links to
  its HTTP request span.
- The handler span carries the MCP semantic-convention attributes.
- With `parent_based_sampler` on, the caller's sampled flag overrides the ratio
  either way. With it off, the ratio decides.
- A JSON log line's `trace_id` finds its trace in Jaeger.
- `mcp.server.operation.duration`, the `apollo.mcp.*` metrics, and the HTTP
  server metrics arrive in Prometheus.

`KNOWN_GAPS` in `run.mjs` lists checks the server is known to fail. They're
reported as `GAP` and don't fail the run, but a known gap that starts passing
does, so the entry gets removed.

Each run writes the server and fixture logs to `artifacts/run-*/`.
