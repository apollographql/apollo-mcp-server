### fix: Include the cargo feature and TraceContextPropagator to send otel headers downstream - @swcollard PR #307

Inside the reqwest middleware, if the global text_map_propagator is not set, it will no op and not send the traceparent and tracestate headers to the Router. Adding this is needed to correlate traces from the mcp server to the router or other downstream APIs