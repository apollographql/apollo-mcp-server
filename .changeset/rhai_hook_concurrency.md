---
default: minor
---

# Run Rhai hooks concurrently instead of behind an exclusive lock

A Rhai hook that blocked on an HTTP call could hang the server. Every hook call took an exclusive lock on the shared engine and held it for the whole script, blocking the waiting calls' tokio worker threads until none was left to finish the in-flight request; three concurrent tool calls were enough on a two-core deployment. Hooks now run against a snapshot of the engine with no lock held, and a reload swaps in a freshly compiled engine so hooks already in flight keep running against the scripts they started with. Hook bodies for concurrent requests now genuinely run in parallel, where the lock used to serialize them, so a hook that performs side effects can no longer assume it is the only one running.

Each hook invocation now gets its own copy of the variables declared at the top level of `main.rhai`. Hooks still read the values captured at load time, but a value a hook writes to one is discarded when the hook returns instead of reaching later calls, the experimental `on_startup` hook included. A script that kept a cached token or a counter there loses those writes with no error and no log, hence the minor bump.

