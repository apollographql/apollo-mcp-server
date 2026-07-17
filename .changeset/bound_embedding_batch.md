---
default: patch
---

# Bound the semantic-search embedding batch to cap memory

Corpus embedding now runs in fixed-size batches instead of one large default
batch. Embedding the whole operation corpus at once inflated transformer-
activation memory to several GB and could OOM a memory-limited container; a
bounded batch caps peak RSS, at the cost of more (smaller) inference passes —
fine for the one-time build-time embed. Also adds a tracing span and a log line
reporting how long corpus embedding took.
