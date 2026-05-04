---
default: patch
---

# Fix file-watcher self-sustaining log loop and watcher-thread starvation

The file-change notifier in `apollo-mcp-registry` retried `mpsc::Sender::try_send`
in a busy loop with `std::thread::sleep(50ms)` whenever the receiver had not yet
drained the previous event. Because the retry runs on the `notify::PollWatcher`
callback thread, a slow consumer can cause the callback to absorb itself in the
sleep/log loop and emit a continuous ~20 Hz stream of `could not process file
watch notification. no available capacity` warnings (issue #743).

The channel has capacity 1 and the consumer only needs to know "something
changed" — duplicate notifications are redundant. Replace the retry loop with a
non-blocking `try_send` that drops a queued duplicate at trace level, and log
the unrecoverable `Closed` case rather than panicking from the watcher thread.
