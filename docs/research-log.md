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

## Session 1 — compaction, hashing, and codegen (2026-08-05)

All runs on Apple M4 Pro (aarch64), RUSTFLAGS `-C target-cpu=native`, gate
`bash .auto/checks.sh` green (fmt, workspace tests, clippy -D warnings, Loom,
frozen-diff) at every keep. Frozen `score` (1M schedule+fire, best of 5):
baseline 4.88 M/s.

### E1 — Bounded heap compaction (keep)
Hypothesis: lazy top-of-heap discard bounds nothing; rebuild the heap from the
generation index once stale entries outnumber live ones (`heap > 2 × live`),
retain + `shrink_to_fit`, O(n). Result: replace retained after drain
706 MB → 75.6 MB (9.3×), cancel stale drain 137 ms → 1.2 ms (111×), cancel
retained 77.6 → 35.7 MB. Score path provably unaffected (no stale ⇒ no
rebuild); observed score delta was machine drift. Primary references:
Sedgewick's *Algorithms* (binary heap rebuild); std `BinaryHeap::from(Vec)`
heapify is O(n).

### E2 — Release buffers after full drain (keep)
Extend compaction: when `current` is empty, `shrink_to_fit` both the heap and
the generation map, so a long-lived queue does not pin a workload's peak
capacity. Result: all `*_retained_bytes_after_*` → 0 (replace, cancel, mixed);
cancel drain → 2.7 µs. Score flat.

### E3 — Amortized schedule-time compaction (keep)
Per-schedule `compact()` call regressed the frozen score −9% (A/B 6.29 vs
5.86 M/s) even though it never fires on the fresh-key path — the per-call
`checked_sub` + branches cost more than predicted. Fix: check every 1024th
schedule via the existing monotonic counter (`n & 1023 == 0`), bounding peak
to 2× live + 1024 entries. Result: replace peak 706.7 → 115.7 B/live (true
transient max 195.6 MB during the shrink double-buffer); schedule regression
gone (A/B −0.7%, noise). **Lesson: hot-path branch cost is measurable even
when never taken; amortize.**

### E4 — FxHash for the generation map (keep)
`HashMap<K, u64>` (SipHash) → `FxHashMap` (`rustc-hash`, the hasher rustc
itself uses). 3 map ops per timer (insert on schedule; get+remove on pop).
Result: score 6.2–6.5 → 7.3–7.5 M/s (+15–18%), pop_due p50 125 → 84 ns,
cancel 24.6 → 73.5 M ops/s (3×), cancel drain 899 µs → 4 µs, mixed +21%.
Hash choice is internal (map never iterated), so observable semantics are
unchanged. Reference: `rustc-hash` (rustc's FxHasher, based on the Fowler
hash variant used by rustc for interned keys).

### E5 — 4-ary heap (FAILED, reverted)
Hypothesis: halve sift depth (log₄ 1M = 10 vs 20) with clustered children.
Custom `Heap` with ARITY=4, same `(at, sequence)` min order; differential
test (50k seeded ops vs BTreeSet/BTreeMap reference model) added and passed.
Result: score 5.57 M/s vs 7.3–7.5 M — **−26%**. ARITY=2 probe measured
7.3–7.7 M (≈ std BinaryHeap), proving the sift implementation sound and the
4-ary branching/division/cache behavior itself the loser on this workload.
Reverted to std BinaryHeap; kept the differential + `&str`-key tests
(cfg(test)-only).

### E6 — Drop `Entry::sequence` / derive from generation (FAILED, reverted)
Hypothesis: `sequence ≡ generation − 1` (both advance exactly once per
schedule from 0/1), so the field and its counter are redundant; 40 B → 32 B
entries (u64 keys). Result: **−12%** (A/B 7.50 vs 6.63 M/s). Root cause found
in disassembly: the frozen workload's u32 keys make the entry 24 B without
`sequence`; 24 B is not a multiple of 16, so LLVM emits multiply-indexing
(`madd` by 24) and split 16 B + 8 B copies in `BinaryHeap::pop`, vs clean
`ldp q0,q1` 32 B vector copies at 32 B. **Lesson: entry size must be a
multiple of 16 B on aarch64 for clean vectorized copies.** Hybrid (keep field,
drop counter) measured statistically identical to HEAD; reverted for
simplicity. `sequence` stays — it is the deterministic equal-deadline
tiebreak.

### E7 — `#[inline]` on hot API (keep, flat)
`schedule`/`pop_due`/`cancel`/`next_deadline`/`compact`/`discard_stale`/
`Entry::cmp` were cross-crate `bl` calls from the perf loop (verified in
disassembly; inlined after the change). Score: single best sample 7.39 M/s,
but interleaved A/B flat (7.15 vs 7.12 M avg, ±7% machine noise). Kept as
standard library practice (other instantiation contexts benefit).

### E8 — `stale_possible` flag (keep)
A bool set by replace-schedule and cancel, cleared by a compaction rebuild
or an emptied heap. `discard_stale` skips its per-pop generation lookup when
the flag is false — exactly the frozen score path (fresh schedules only).
Cancellation semantics preserved (get-then-remove — an early
`remove().is_some()` variant would have cancelled stale tokens; caught by the
differential test before the gate). Result: +2.2% A/B (7.67/7.68 vs
7.96/7.71 M avg).

### E9 — Thin LTO + single codegen unit (keep)
`[profile.release] codegen-units = 1, lto = "thin"`. Inlines std
`BinaryHeap` sift internals into the hot loop (previously cross-crate
`bl` calls into std monomorphizations). A/B decisive: 7.61/7.59 vs
8.12/8.09 M avg = **+6.7%**. `lto = "fat"` measured −1% vs thin and was
reverted.

### Session totals
`score` 4.88 → ~8.1 M/s (+66% nominal; real gains: FxHash ~+15%, LTO ~+7%,
stale flag ~+2%, remainder machine-state drift). replace peak 706.7 →
115.7 B/live, all retained-after-drain/cancel → 0, cancel drain 137 ms →
~3 µs (40,000×), cancel throughput 19.8 → 103 M ops/s (5×), pop_due p50
125 → 84 ns. Memory story is closed for the array-heap core; score is within
~10% of the measured ceiling for schedule+pop on this machine.

### Algorithmic survey (for future adapters, out of core)
The generic core is pinned to `I: Copy + Ord` (frozen tests use `u64` and
`&str`), so numeric-instant structures are adapters, not core replacements.
- Varghese & Lauck, *Hashed and Hierarchical Timing Wheels* (1997) — O(1)
  schedule/expire for bounded deadline ranges; the 65 536-deadline frozen
  workload is a natural wheel fit, but bucket count depends on the instant
  type's range.
- Brown, *Calendar Queues* (1988) — bucketed with resizing; O(1) expected.
- Ahuja, Mehlhorn, Orlin & Tarjan, *Faster Algorithms for the Shortest Path
  Problem* (1990) — radix heaps: O(log C) with integer keys.
- Sedgewick, indexed priority queue — decrease-key via key→position map.
- Netty `HashedWheelTimer`; Kafka Purgatory (hierarchical wheels with
  bucket-level timers); Linux `timer_wheel.c` (hierarchical, 5 levels).
- Pairing heaps (Fredman, Sedgewick, Sleator, Tarjan 1986): O(1) push but
  degenerate on near-sorted insertions (the min root accumulates O(n)
  children → O(n) pop) — measured-reasoned against, not implemented.
