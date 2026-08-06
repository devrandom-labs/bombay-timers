# Timepass adversarial test-only autoresearch

Write only under `research/timepass-autoresearch/**`. Never touch or fix
production, manifests, existing tests, docs, `.auto`, or the launcher.

Attack keyed replacement, stale cancellation and expiration, at-most-once
delivery, never-early semantics, equal-deadline determinism, generation and
sequence exhaustion, zero/extreme instants, heap/map divergence, stale-entry
churn, move-only value drops, memory retention and reclamation. Use an
independent scheduler model, exhaustive histories, proptest, fuzzing,
deterministic stress, bounded Loom only for adapters/concurrent protocols, and
Miri for ownership validity.

Minimize every defect into `#[ignore = "FINDING-NNN: reason"]`, record
`## FINDING-NNN` in `RESEARCH-REPORT.md` with exact command/seed and expected
versus actual behavior, then continue testing. Never fix it. Keep passing tests
active and report all bounds and interrupted runs honestly.
