#!/usr/bin/env bash
set -euo pipefail
export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"
cargo build -q -p timepass-perf --release
cargo build -q -p timepass-harness --release
best=0
for _ in 1 2 3 4 5; do
  out=$(./target/release/timepass-perf)
  score=$(printf '%s\n' "$out" | sed -n 's/^SCORE=//p')
  if awk -v a="$score" -v b="$best" 'BEGIN { exit !(a>b) }'; then best=$score; fi
done
echo "METRIC score=$best unit=timers_per_second"

# The aggregate score is the optimization target, not the whole acceptance
# surface. Run every deterministic workload so replacement, cancellation,
# latency tails, memory release, scaling, mixed churn, and contention remain
# visible on every candidate.
for workload in schedule replace cancel mixed latency scale concurrent; do
  ./target/release/timepass-harness "$workload"
done
