---
default: patch
---

# Fix unused import warning on Windows targets

The `tracing::error` macro was imported at the top level of `main.rs` but only used inside a `#[cfg(unix)]` block, causing an unused import warning that fails the release build on Windows targets with `--deny warnings`. The import is now scoped to the unix-only call site.
