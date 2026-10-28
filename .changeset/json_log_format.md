---
default: minor
---

# Add JSON log output and `span_id` to log lines

A new `logging.format` option selects the log output format. `text` (the default) keeps today's human-readable output. `json` writes one JSON object per line, with `timestamp`, `level`, `target`, the current span's name as `span`, and the event's fields as top-level keys, so log pipelines can parse lines without regular expressions. JSON output never contains ANSI styling.

Log lines emitted inside a span now include the span's `span_id` next to its `trace_id`. In `text` format, the prefix is `trace_id=<hex> span_id=<hex>`. In `json` format, both are top-level keys. Lines outside a span have neither. Existing deployments see no change other than the added `span_id`.
