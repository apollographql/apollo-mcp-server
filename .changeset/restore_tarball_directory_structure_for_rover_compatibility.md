---
default: patch
---

### Restore tarball directory structure for Rover compatibility - @DaleSeo PR #563

Fixes a regression that broke Rover's ability to download and run the MCP server binary.

When the release workflow was rewritten recently, the tarball directory structure was changed. The original NIX-based workflow packaged the binary at dist/, but the new workflow put it under artifact/ instead.