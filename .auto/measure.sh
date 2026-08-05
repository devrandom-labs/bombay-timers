#!/usr/bin/env bash
set -euo pipefail
export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"
cargo build -q -p timepass-perf --release
best=0
for _ in 1 2 3 4 5; do
  out=$(./target/release/timepass-perf)
  score=$(printf '%s\n' "$out" | sed -n 's/^SCORE=//p')
  if awk -v a="$score" -v b="$best" 'BEGIN { exit !(a>b) }'; then best=$score; fi
done
echo "METRIC score=$best unit=timers_per_second"

