---
default: patch
---

# Fix config hot reload crashing server on invalid config

The config hot-reload feature was supposed to keep the server running when an invalid config file was saved, but instead the server would crash because it cancelled the running instance before validating the new config. The new config is now validated before the server is stopped, and if validation fails, the error is logged and the server continues running with the previous configuration.
