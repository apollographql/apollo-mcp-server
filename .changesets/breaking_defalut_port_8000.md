### Change default port from 5000 to 8000 - @DaleSeo PR #417

The default server port has been changed from `5000` to `8000` to avoid conflicts with common development tools and services that typically use port 5000 (such as macOS AirPlay, Flask development servers, and other local services).

**Migration**: If you were relying on the default port 5000, you can continue using it by explicitly setting the port in your configuration file or command line arguments.

- Before 

```yaml
transport:
  type: streamable_http
```

- After

```yaml
transport:
  type: streamable_http
  port: 5000
```