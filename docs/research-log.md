# Research log

Record hypothesis, primary sources, implementation, Loom/Miri results,
benchmark distribution, allocations, retained memory, and decision for every
experiment. The initial binary heap plus hash generation index is a semantic
baseline, not a preferred algorithm.

## Baseline — binary heap + HashMap generation index (2026-08-05)

Implementation: `crates/timers/src/lib.rs` — `BinaryHeap<Reverse<Entry>>`
min-heap ordered by `(at, sequence)`, `HashMap<K, u64>` key → current
generation. Stale entries are lazily discarded from the heap on
`next_deadline`/`pop_due`. M1 runs of `crates/timers-harness` (RUSTFLAGS
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
`the Nix verification gate green (fmt, workspace tests, clippy -D warnings, Loom,
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

### E10 — Pipelined custom sift-down (FAILED, reverted)
Hypothesis: the pop sift is latency-bound (~19 dependent levels ≈ 100 ns of the
114 ns pop, measured by phase-split probes: schedule 21 ns, pop 114 ns =
compact 0–3 + peek 1 + sift ~100 + map remove 6 + build 2). A custom binary
heap carrying `(at, sequence)` keys in registers and issuing the grandchildren
loads one level ahead (safe Rust, swap-based descent) would hide load latency.
Result: pop 114 → 164 ns (**+44%**), score 8.1 → 5.4 M/s (−34%). Root cause:
the swap-based descent costs 3 moves per level (2 loads + 2 stores of 32 B)
versus std's hole abstraction (1 move per level, `ptr::read`/`write` — unsafe
internally, gated behind the repo's Loom+Miri rule, and Miri needs nightly,
which the stable-toolchain pin forbids). The extra memory traffic dominates the
latency savings. **Lesson: std `BinaryHeap`'s hole sift is the safe-Rust
ceiling for comparison-based heaps; the sift (~100 ns of the 114 ns pop) is not
further reducible without unsafe.** Probes also priced the map: insert+remove
~11 ns of the 135 ns/timer total.

### E11 — GenMap: purpose-built linear-probe map (keep)
Replace `FxHashMap<K, u64>` (hashbrown Swiss table) with `GenMap<K>`: three
parallel arrays (1 B state byte + `Option<K>` + `u64` generation), linear
probing with tombstones over power-of-two capacity, raw `FxHash` (no h2
control-byte split). Score **+9–10%** (two same-window A/Bs: 7.91 vs 8.62 M/s
and 8.50 vs 9.34 M/s) — one state-byte load per probe beats the 16 B SIMD
control-group scan on the narrow insert/get/remove contract. A single-probe
`cancel(key, gen)` method replaced get-then-remove: cancel throughput
98 → 140 M ops/s (+42%). Costs: replace peak 115.7 → 132.4 B/live (+14%, still
5.3× better than baseline 706), allocs 2–3× (tiny absolute).

**Bug caught by the mixed workload (600× slowdown)**: the growth condition
counted only occupied entries, so tombstones from many distinct keys could
fill the probe table (no empty slot ⇒ probes never terminate). Fixed by
counting tombstones toward the load factor (`len + tombstones + 1 >
cap·3/4` triggers a rebuild that drops tombstones). The differential test now
ends with a schedule-then-cancel sweep (1024 keys × 2000 rounds) that provably
hangs on the buggy condition — verified twice. Lesson: linear-probe tables
must treat tombstones as occupied for both probe termination and growth.

### E12 — GenMap backward-shift deletion (keep)
Replace the tombstone scheme with backward-shift deletion: removing an entry
walks forward and shifts any following entry whose probe chain passes through
the vacated slot (condition: hash start `h` satisfies `h ≤ i ≤ j`, wrapping).
No tombstones, no state array (Option-ness is the occupancy marker), chains
stay short, growth counts only occupied slots. Score flat (A/B 6.04 vs
6.01 M/s — gate neutral). Mixed **+6%** (21.9 vs 20.7 M ops/s — the
tombstone-triggered rebuild spikes are gone), allocs **2× fewer** (mixed
0.000027 → 0.000013, a first-class metric), peak **−2%** (replace
132.4 → 130.3, schedule 94.4 → 92.3 B/timer). Cancel **−31%** on the dense
mass-cancel synthetic (134 → 93 M ops/s — the shift walk vs a deferred
tombstone write; still 4.7× baseline; real cancellation is spread, and the
realistic mixed workload improved). The 4-seed differential caught an
early-stop bug in the first formulation (breaking the walk at a home-placed
entry left later entries unreachable); the chain-passes-gap condition fixes it.

### E13 — GenMap cached growth threshold (keep)
The per-insert growth check recomputed `(capacity() * 3) / 4` on every call;
cache it as a `grow_threshold` field updated only on rebuild (and reset on
shrink). `len >= threshold` is exactly equivalent to `len + 1 > cap·3/4`
(`threshold = 0` on an empty table triggers the first grow). Score **+2.0%**
(A/B 8.22 vs 8.06 M/s avg) — the hot insert drops a capacity load, a
multiply, and a zero-check. Gate green.

### E14 — Gate `discard_stale`'s compaction on the staleness flag (keep)
The clean path (fresh schedules, no cancels) ran `compact()` on every pop:
a `checked_sub`, a staleness comparison, and an emptiness check — pure
overhead when no stale entry can exist. `discard_stale` now returns
immediately when `stale_possible` is clear, and the drain-release (shrink
heap + map when `current` empties) moved into `pop_due`'s removal, where the
emptiness check runs once per pop anyway. Memory semantics preserved and
verified across the matrix (retained 0 everywhere, peaks identical: the
drain-release fires on the clean path's last pop and on the stale path
inside `compact`). Score **+0.9%** (A/B 8.58 vs 8.50 M/s avg). Gate green.

### E15 — Pipelined hole sift (FAILED, reverted; Miri gate discovered)
The unsafe gate was assumed unreachable ("Miri needs nightly, pinned out") —
wrong: the repo's `flake.nix` ships `devShells.miri` (nightly + miri). A custom
binary heap with a hole-based, software-pipelined sift was implemented with
documented invariants and validated by Loom (frozen gate) and Miri (all 4
tests clean in 265 s under the interpreter). Design: element carried in a
register (1 move/level), children's `(at, sequence)` keys carried in
registers, grandchildren loads issued one level ahead of the min-selection.
Result: score **−17%** (A/B 8.04 vs 6.70 M/s). The 7 carried key tuples
(14 registers) plus the 32 B element spill on the M4, and the 4 early
grandchild loads per level add traffic std avoids. **Third sift attempt lost
to std's LTO-optimized hole sift** (4-ary −26%, swap-pipeline −34%, this −17%);
std `BinaryHeap` is the empirical optimum for this workload on this machine.
The `nix develop .#miri` capability is recorded: unsafe with invariants +
Loom + Miri evidence is permissible in future work.

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
