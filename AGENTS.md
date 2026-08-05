# Timepass research rules

Timepass is an actor-independent keyed monotonic scheduler. It knows keys,
ordered instants, generations, values, cancellation, and expiration. It must not
know actors, Tokio, cron strings, time zones, supervision, KERI, Zenoh, or Nexus.

Frozen semantics are inviolable: at most one current generation per key; stale
tokens cannot cancel replacements; stale heap entries never fire; expiration is
at most once; equal-deadline order is deterministic; no item fires early. Never
weaken tests or measurements. Research primary papers and production timer
implementations, record failures, and prefer safe Rust. Unsafe requires explicit
invariants plus Loom and Miri evidence.

