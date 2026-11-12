---
default: minor
---

# Rate limit failed authentications per client

The new `transport.auth.rate_limit.per_client` option gives each client address its own rate-limit bucket, alongside the bucket for each bearer credential. A client that sends a new fabricated token on every request is now limited too. The client bucket uses the same `requests_per_second` and `burst`, and counts only requests that fail authentication. An IPv6 client is limited by its `/64` prefix. Behind a reverse proxy or load balancer, list it under `transport.trusted_proxies` so the limit applies to the original client instead of the proxy. The option is off by default, because without `trusted_proxies`, every client behind a proxy shares one bucket.

```yaml
transport:
  type: streamable_http
  trusted_proxies:
    - 10.0.4.0/24
  auth:
    rate_limit:
      per_client: true
```
