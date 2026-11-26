---
default: patch
---

# Keep the server running when a reloaded configuration fails at startup

With `streamable_http` transport, the server restarts in place when its configuration file changes. Before, it only checked that the new file parsed. A file that parsed but failed a later startup check stopped the server, for example when `apps.expose_without_app_param` listed a misspelled tool name.

The server now runs every startup check on the new configuration before it stops. If a check fails, the server logs the error and keeps running with its current configuration. It still stops if it can't bind the new `address` or `port`, because it can't check them until it restarts.

A second `SIGHUP` now reloads the configuration again. Before, only the first `SIGHUP` after a start or restart had an effect.
