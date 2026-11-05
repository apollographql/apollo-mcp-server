---
default: minor
---

# Record the original client behind trusted proxies

The new `transport.trusted_proxies` option lists the reverse proxies and load balancers in front of the server, as CIDR ranges or single IP addresses. With `telemetry.exporters.tracing.client_address` enabled, a request from a trusted proxy records the client it names in `X-Forwarded-For` as `client.address`, instead of the proxy's address. `network.peer.address` and `network.peer.port` still record the proxy. The server ignores `X-Forwarded-For` from any other peer, so callers can't choose their recorded address. The list is empty by default, which keeps the current behavior.

```yaml
transport:
  type: streamable_http
  trusted_proxies:
    - 10.0.4.0/24 # the load balancer subnet
```
