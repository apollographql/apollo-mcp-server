---
default: minor
---

# Record client addresses on spans (opt-in)

Set `telemetry.exporters.tracing.client_address: true` to record the client's IP address on traces. The HTTP request span records `client.address`, `network.peer.address`, and `network.peer.port`, and MCP handler spans record `client.address` and `client.port`, as the OpenTelemetry HTTP and MCP semantic conventions recommend. Requests over stdio have no client address.

The option is off by default because an IP address is personal data in many jurisdictions. Metrics never carry these attributes. By default, the address comes from the TCP connection, so behind a reverse proxy or load balancer it is the proxy's address. To record the original client instead, list your proxies under `transport.trusted_proxies`.
