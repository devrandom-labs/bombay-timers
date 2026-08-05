# Research log

Record hypothesis, primary sources, implementation, Loom/Miri results,
benchmark distribution, allocations, retained memory, and decision for every
experiment. The initial binary heap plus hash generation index is a semantic
baseline, not a preferred algorithm.

## Baseline — binary heap + HashMap generation index (2026-08-05)

Implementation: `crates/timepass/src/lib.rs` — `BinaryHeap<Reverse<Entry>>`
min-heap ordered by `(at, sequence)`, `HashMap<K, u64>` key → current
generation. Stale entries are lazily discarded from the heap on
`next_deadline`/`pop_due`. M1 runs of `crates/timepass-harness` (RUSTFLAGS
`-C target-cpu=native`, release):

| workload | metric | value |
|---|---|---|
| schedule 1M | timers/s | 3.81 M |
| schedule | allocs/timer | 0.000039 (~21 reallocs per 1M) |
| schedule | peak bytes/timer | 77.6 |
| schedule | retained after drain | 77.6 MB (capacity never shrinks) |
| replace 1M×8 | ops/s | 1.42 M |
| replace | peak bytes/live | 707 |
| replace | retained after drain | 706 MB (9M stale entry slots) |
| cancel 1M | cancels/s | 19.8 M |
| cancel | allocs/cancel | 0 |
| cancel | retained after cancel | 77.6 MB |
| cancel | stale drain 1M entries | 137 ms |
| mixed sim (200k actors, 2M steps) | ops/s | 16.7 M |
| mixed | peak step fires (bursts) | 1002 |
| mixed | retained after drain | 116 KB (sparse ~275-entry working set) |
| latency replace | p50/p99 | 42 / 250 ns |
| latency cancel | p50/p99 | 42 / 167 ns |
| latency pop_due (1M heap) | p50/p99 | 542 / 1459 ns |
| scale 1k / 100k / 1M | timers/s | 9.7 M / 7.6 M / 3.9 M |
| concurrent 8 threads | ops/s | 9.5 M |

Key observations to exploit:
- pop_due at 1M population costs ~0.5–1.5 µs (log-n); schedule on a 1M queue
  costs ~40–250 ns; cancellation is cheap but leaves O(n) retained capacity.
- Replace-heavy churn retains 706 MB of stale slots after drain — the dominant
  memory problem; the actorpass pattern is replace-heavy.
- `schedule_allocs_per_timer` amortizes to ~0 because Vec/HashMap double; a
  design that allocates per schedule would show ~2.0 here.
- Frozen metric harness measures schedule+fire only (`score` ≈ 4.8 M/s on the
  1M mirror); the richer matrix above is what decides algorithm choice.
