---
default: patch
---

# Serve a single-target app's resource when the URL has no `appTarget`

Reading the resource of an app built for only one target failed with `Invalid apps target: no resource found for openai` (or `mcp`) unless the URL named that target. When the URL has no `appTarget`, the server now serves the one resource the app has. A target named in the URL must still match.
