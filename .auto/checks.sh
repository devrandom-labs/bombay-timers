#!/usr/bin/env bash
set -euo pipefail
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTFLAGS="--cfg loom -D warnings" LOOM_MAX_PREEMPTIONS=3 cargo test -p timepass --test loom --release
git diff --quiet baseline -- crates/timepass/tests crates/timepass/benches crates/timepass-perf crates/timepass-harness .auto/checks.sh .auto/measure.sh .auto/prompt.md || {
  echo "CHECK FAIL: frozen semantics or measurement changed"; exit 1;
}
echo "CHECK OK"
