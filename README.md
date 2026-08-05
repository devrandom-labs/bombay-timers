# timepass

A generic keyed monotonic scheduler for actorpass. The initial binary heap plus
generation index is a correctness and measurement baseline only.

Timepass accepts generic ordered instants. Tokio clocks, standard clocks, and
virtual clocks belong in adapters. Cron/calendar parsing, time zones, durable
misfire policy, and replay belong above the core—typically in Nexus plus a
calendar adapter.

```text
schedule(key, generation, at, value)
cancel(exact token)
next_deadline()
pop_due(now)
```

Run `.auto/checks.sh`; use `.auto/prompt.md` with OMP `/autoresearch`.

