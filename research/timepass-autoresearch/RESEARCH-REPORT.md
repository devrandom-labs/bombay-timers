# Timepass adversarial test report

Target: `timepass` 0.1.0 at baseline commit `ecdd03c` (branch
`autoresearch/run-the-repository-s-test-only-adversarial-autor-20260806`).
Production code, manifests, existing tests, docs, `.auto`, and the launcher
are untouched; everything below lives under
`research/timepass-autoresearch/**` and is test infrastructure only.

Invariants attacked: keyed replacement, stale cancellation and expiration,
at-most-once delivery, never-early semantics, equal-deadline determinism,
generation/sequence exhaustion, zero/extreme instants, heap/map divergence,
stale-entry churn, move-only value drops, memory retention and reclamation.

## Infrastructure

- `src/lib.rs` — `World<K>`: an independent reference model (`BTreeMap` +
  `BTreeSet` ordered by `(at, seq)`, sharing no code with the implementation)
  wrapped around the queue under test. Every op is followed by observable
  checks (`len`, `is_empty`, `next_deadline`); `drain_check` verifies the
  complete firing order, value integrity, post-drain emptiness, and that every
  outstanding token is inert. Also: `CollideKey` (constant-hash key forcing a
  single probe chain), `DropValue`/`DropLog` (move-only values with exact
  drop accounting and double-drop detection), `Rng` (deterministic
  xorshift64*), and `run_bytes` (the byte→op grammar shared by the
  deterministic campaign and the fuzz targets).
- `tests/differential.rs` — five proptest suites against `World`.
- `tests/exhaustive.rs` — exhaustive small-state exploration with a
  transposition table over canonical behavioral states; every visited state is
  extended by every op and fully drain-verified.
- `tests/adversarial.rs` — targeted attacks (below).
- `tests/memory.rs` — counting-allocator retention/reclamation tests,
  serialized with a mutex because the counter is process-global.
- `tests/byte_campaign.rs` + `src/bin/byte_campaign.rs` — deterministic
  seeded byte-stream campaign (the fuzz grammar without the fuzzer).
- `fuzz/fuzz_targets/{scheduler_ops,scheduler_ops_collide}.rs` — two
  coverage-guided libFuzzer targets driving `run_bytes`.

## Campaign results

### Differential proptest (native, debug; `cargo test --all-targets`)

| suite | cases | ops/case | domain |
|---|---|---|---|
| `differential_small_domain` | 512 | 1–200 | 4 keys, deadlines weighted to 0–16 + {0, 1, MAX−1, MAX} |
| `differential_wide_domain` | 256 | 1–400 | 64 keys, any u64 instant |
| `differential_replace_heavy_churn` | 64 | 1500–2500 | 8 keys, schedule-weighted; crosses the 1024-schedule compaction boundary repeatedly |
| `differential_cancel_heavy` | 256 | 1–600 | 8 keys, cancel-weighted |
| `differential_colliding_keys` | 256 | 1–300 | 16 constant-hash keys (single probe chain) |
| `differential_extreme_ties` | 256 | 1–300 | 6 keys, instants only at {0, 1, MAX−1, MAX} — ties decided by sequence at the domain edges |
| `differential_pop_heavy` | 256 | 1–400 | 8 keys, pop-weighted; constant fill/drain/refill cycling |

All pass. No proptest failure seeds were persisted (no `proptest-regressions`
files exist). Proptest runs are deterministic for a fixed proptest version and
config; the gate replays exactly these configurations.

### Exhaustive small-state exploration

Complete for each domain (transposition table prunes to the full reachable
canonical state space; the depth cap of 400 is a termination guard — natural
completion depths are all far below it):

| domain | states | edges checked | completion depth |
|---|---|---|---|
| 2 keys, ats/nows {0,1,2} | 76 | 988 | 26 |
| 2 keys, ats/nows {0, MAX−1, MAX} | 76 | 988 | 26 |
| 3 keys, ats/nows {0,1} | 392 | 5488 | 69 |
| 3 keys, ats/nows {0,1,2} | 848 | 15,264 | 133 |
| 3 keys, at {7}, nows {0,7} (max ties) | 128 | 1408 | 31 |

Every visited state passes stepwise observable checks and a full
drain-order verification. The 3-key domains run natively only (Miri time).

### Targeted adversarial tests

14 active tests pass: never-early sweeps over 0–64 and {MAX−1, MAX} (including
with stale entries lurking below the live deadline), 1000-way equal-deadline
ordering, replacement reorders to the back at equal deadlines, single-key
replacement across three compaction boundaries with 3073-token staleness
replay, deterministic churn (3 fixed seeds × 100,000 model-checked ops; 2,000
under Miri), 100-generation stale-token inertness, move-only drop accounting
through replace/cancel/pop and through queue drop, i64 instant extremes, 50
fill/drain cycles, cloned-token single-use authority, `next_deadline`
idempotence across stale discards, and cancel-then-reschedule at the same
instant.

### Deterministic byte-stream campaign

- Gate (debug): 2 seeds × 4,096 streams, max 1,024 bytes/stream — passes in ~0.2 s.
- Release run: `ADV_STREAMS=1000000 ADV_MAX_LEN=4096 cargo run --release --bin byte_campaign`
  — 1,000,000 streams (seed `0x5eed5eed`, ~205 ops/stream average, ≈2×10^8
  model-checked operations, alternating narrow/wide payloads and u64/colliding
  key spaces) completed in 19.65 s with zero divergence.

### Coverage-guided fuzzing

cargo-fuzz 0.13.2 (via `nix shell nixpkgs#cargo-fuzz`) on the repo's nightly
shell (`nix develop .#miri`), libFuzzer with sanitizer coverage:

- `cargo fuzz run scheduler_ops --fuzz-dir fuzz -- -max_total_time=90`
  → 2,432,546 executions in 91 s, no crash.
- `cargo fuzz run scheduler_ops_collide --fuzz-dir fuzz -- -max_total_time=60`
  → 1,327,620 executions in 61 s, no crash.

Final corpuses: 914 and 803 inputs respectively (regenerable; `fuzz/corpus`
and `fuzz/artifacts` are gitignored, not committed). Crash inputs would
reproduce deterministically through `run_bytes` with the input bytes.

### Miri

`nix develop .#miri -c cargo miri test --manifest-path research/timepass-autoresearch/Cargo.toml`
(fenix nightly + miri, aarch64-apple-darwin): all non-gated suites pass under
the interpreter in ~408 s — exhaustive 2-key domains, targeted adversarial
tests (churn reduced to 3×2,000 ops), byte campaign (64 streams/seed), and the
counting-allocator memory tests. The proptest suites are `cfg(not(miri))`-gated
(randomized coverage is native-only; the deterministic suites carry the Miri
evidence). No ownership-validity or drop errors.

### Loom

Not applicable, with justification: `TimerQueue` exposes only `&mut self`
methods and contains no atomics, no `unsafe`, and no concurrent protocol — it
is a single-threaded data structure. The prompt scopes Loom to
"adapters/concurrent protocols"; this crate ships neither. The production
crate's own `tests/loom.rs` covers the only shared-state toy protocol present.

### Generation/sequence exhaustion

Not directly testable: `next_generation`/`next_sequence` are `u64` counters
advanced once per `schedule` with `checked_add(...).expect(...)`. Reaching the
panic requires 2^64 schedules ≈ 73,000 years at the measured ~8 M schedules/s
native rate. The panic-on-exhaustion path is therefore attested by code
reading only, not executed — recorded here as an honest coverage bound.

### Interrupted runs

None. All campaigns above ran to their stated completion.

### Harness notes

- `.auto/measure.sh` counts findings via `rg -c '^## FINDING-'` on the single
  report file and parses the output as `path:count`; `rg -c` on one explicit
  file prints a bare count, so the findings metric registers 0 regardless of
  the report's contents. FINDING-001 below is fully recorded; the
  under-counting is noted here because `.auto` is immutable per the campaign
  rules.
- Stale-token probes in `World::cancel_stale` check the oldest and newest
  retired tokens for a key (the oldest is where generation aliasing would
  surface first); the deterministic churn and all property suites exercise
  this on every stale-cancel op.

## FINDING-001 — tokens carry no queue identity; cross-queue cancellation succeeds

- **Severity:** low (API authority semantics; no memory unsafety, no
  single-queue semantic violation). Affects any deployment running more than
  one `TimerQueue` over a shared key space.
- **Affected version:** 0.1.0 @ `ecdd03c`.
- **Expected:** `Token` is documented as "exact authority over one scheduled
  generation". Authority over *one* generation implies isolation: a token
  minted by queue A must not cancel a schedule in queue B, even when key and
  generation number coincide.
- **Actual:** `Token<K>` is `(key, generation)` with no queue identity, and
  both queues start `next_generation` at 1, so the first token minted by queue
  A cancels the first schedule of the same key in queue B. The cancel path
  matches `(key, generation)` against B's generation map and succeeds.
- **Minimized reproducer:** `tests/adversarial.rs::cross_queue_tokens_must_not_cancel`
  (4 API calls; ignored with `#[ignore = "FINDING-001: ..."]`, asserting the
  correct isolated behavior).
- **Reproduction command:**
  `cargo test --manifest-path research/timepass-autoresearch/Cargo.toml --test adversarial -- --ignored cross_queue_tokens_must_not_cancel`
  → panics with `FINDING-001: queue A's token cancelled queue B's generation`.
- **Seed/input:** none required (deterministic 4-call sequence).
- **Note:** per the campaign rules this is recorded, not fixed. If the
  intended contract is "tokens are only meaningful for the queue that minted
  them", the defect is a documentation/specification gap at minimum; the
  current doc comment claims exact authority, which the type does not enforce.
