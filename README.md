# Bombay Timers

`bombay-timers` is a keyed, generation-safe monotonic timer queue. The published
package is `bombay-timers`; its Rust library name is `timers`.

```toml
[dependencies]
bombay-timers = "0.1"
```

```rust
use bombay_timers::TimerQueue;

let mut timers = TimerQueue::new();
let stale = timers.schedule("retry", 10_u64, "first");
let current = timers.schedule("retry", 20, "replacement");

assert!(!timers.cancel(&stale));
assert_eq!(timers.next_deadline(), Some(20));
assert_eq!(timers.pop_due(20).unwrap().value, "replacement");
assert!(!timers.cancel(&current));
```

Instants are generic ordered values. Clock driving, sleeping, calendar rules,
and delivery policy belong in adapters. Tokens are queue-branded exact
cancellation authority, so a token from another queue cannot cancel anything.

## Verification

```bash
nix flake check -L
nix build .#coverage -L
```

The reference models, exhaustive explorer, property tests, fuzz targets, replay
corpus, memory checks, and findings record live under `research/timers-tests/`.

See the [guide](https://docs.page/devrandom-labs/bombay-timers) and
[API reference](https://docs.rs/bombay-timers).

Licensed under Apache-2.0 or MIT, at your option.
