//! Targeted adversarial tests: compaction boundaries, instant extremes,
//! never-early sweeps, equal-deadline determinism, stale-token replay,
//! move-only value drop accounting, and cross-queue token authority.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use timepass::TimerQueue;
use timepass_autoresearch::{DropLog, Op, Rng, World};

/// Never-early: a timer must not fire before its instant, at every instant,
/// including the edges of the domain.
#[test]
fn never_early_sweep() {
    for at in 0..=64_u64 {
        let mut queue = TimerQueue::new();
        queue.schedule("k", at, at);
        if at > 0 {
            assert_eq!(queue.pop_due(at - 1), None, "fired early at {at}");
        }
        let fired = queue.pop_due(at).expect("must fire at its instant");
        assert_eq!(fired.value, at);
        assert!(queue.is_empty());
    }
    for at in [u64::MAX - 1, u64::MAX] {
        let mut queue = TimerQueue::new();
        queue.schedule("k", at, at);
        assert_eq!(queue.pop_due(at - 1), None, "fired early at {at}");
        assert_eq!(queue.pop_due(at).expect("must fire").value, at);
    }
    // Zero instant fires immediately at now = 0.
    let mut queue = TimerQueue::new();
    queue.schedule("k", 0_u64, "zero");
    assert_eq!(queue.pop_due(0).expect("zero instant").value, "zero");
}

/// Never-early with a stale entry lurking below the live deadline: the stale
/// entry's earlier instant must not surface as a deadline or a firing.
#[test]
fn never_early_with_stale_below_live() {
    let mut queue = TimerQueue::new();
    let old = queue.schedule("k", 5_u64, "old");
    queue.schedule("k", 10, "new");
    assert!(!queue.cancel(&old));
    assert_eq!(queue.next_deadline(), Some(10));
    assert_eq!(queue.pop_due(7), None);
    assert_eq!(queue.pop_due(9), None);
    assert_eq!(queue.pop_due(10).expect("due").value, "new");
    // Cancelled earlier deadline must not resurrect.
    let mut queue = TimerQueue::new();
    let token = queue.schedule("k", 3_u64, "cancelled");
    queue.schedule("other", 8, "other");
    assert!(queue.cancel(&token));
    assert_eq!(queue.next_deadline(), Some(8));
    assert_eq!(queue.pop_due(7), None);
    assert_eq!(queue.pop_due(8).expect("due").value, "other");
}

/// Equal deadlines fire in exact schedule order, at scale.
#[test]
fn equal_deadline_mass_fires_in_schedule_order() {
    let mut queue = TimerQueue::new();
    let n = 1_000_u64;
    for key in 0..n {
        queue.schedule(key, 42_u64, key);
    }
    assert_eq!(queue.len(), n as usize);
    for key in 0..n {
        assert_eq!(
            queue.pop_due(42).expect("all due").value,
            key,
            "equal-deadline firing order broke at {key}"
        );
    }
    assert!(queue.is_empty());
}

/// Replacing a key at the same instant moves it behind its peers (the
/// replacement carries a newer sequence).
#[test]
fn equal_deadline_replace_moves_to_back() {
    let mut queue = TimerQueue::new();
    queue.schedule("a", 10_u64, "a1");
    queue.schedule("b", 10, "b");
    queue.schedule("c", 10, "c");
    queue.schedule("a", 10, "a2");
    let order: Vec<&str> = std::iter::from_fn(|| queue.pop_due(10))
        .map(|expired| expired.value)
        .collect();
    assert_eq!(order, ["b", "c", "a2"]);
}

/// One key replaced past several compaction boundaries: exactly one live
/// schedule throughout, only the newest value ever fires.
#[test]
fn replace_past_compaction_boundaries_single_key() {
    let mut queue = TimerQueue::new();
    let mut last_at = 0_u64;
    let mut tokens = Vec::new();
    for round in 0..3 * 1024_u64 {
        last_at = round % 97;
        tokens.push(queue.schedule("k", last_at, round));
        if round % 512 == 0 {
            assert_eq!(queue.len(), 1, "replacement must keep one live entry");
        }
    }
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.next_deadline(), Some(last_at));
    // Every superseded token is stale; only the newest cancels.
    for token in &tokens[..tokens.len() - 1] {
        assert!(!queue.cancel(token), "superseded token cancelled");
    }
    let fired = queue.pop_due(last_at).expect("due");
    assert_eq!(fired.value, 3 * 1024 - 1, "only the newest value may fire");
    assert!(queue.is_empty());
    assert!(!queue.cancel(tokens.last().expect("nonempty")));
}

/// Deterministic replace/cancel/pop churn, model-checked, crossing many
/// compaction boundaries. Seeds are fixed; reported in RESEARCH-REPORT.md.
#[test]
fn deterministic_churn_against_model() {
    let ops_total: u64 = if cfg!(miri) { 2_000 } else { 100_000 };
    let space: Vec<u64> = (0..32).collect();
    for seed in [0xC0FF_EE01, 0xBAD5_EED5, 0xFACE_0003] {
        let mut rng = Rng(seed);
        let mut world = World::new();
        for _ in 0..ops_total {
            let key = rng.below(32) as usize;
            let op = match rng.below(20) {
                0..=10 => Op::Schedule {
                    key,
                    at: rng.below(256),
                },
                11..=14 => Op::CancelCurrent { key },
                15..=16 => Op::CancelStale { key },
                _ => Op::Pop {
                    now: rng.below(256),
                },
            };
            world.apply(&space, op);
        }
        world.drain_check();
    }
}

/// One hundred generations of one key: every retired token is inert before
/// and after the current one cancels.
#[test]
fn stale_tokens_inert_across_many_generations() {
    let mut queue = TimerQueue::new();
    let mut tokens = Vec::new();
    for round in 0..100_u64 {
        tokens.push(queue.schedule("k", 1_000, round));
    }
    for token in &tokens[..99] {
        assert!(!queue.cancel(token));
    }
    assert!(queue.cancel(&tokens[99]));
    assert!(queue.is_empty());
    for token in &tokens[..99] {
        assert!(!queue.cancel(token));
    }
    assert!(!queue.cancel(&tokens[99]));
    assert_eq!(queue.pop_due(u64::MAX), None);
}

/// Move-only values: every value created is dropped exactly once across
/// replacement, cancellation, firing, and queue drop.
#[test]
fn move_only_values_dropped_exactly_once_lifecycle() {
    let log = Arc::new(DropLog::default());
    {
        let mut queue = TimerQueue::new();
        let t0 = queue.schedule(0_u64, 5_u64, log.value(0));
        let _t1 = queue.schedule(1_u64, 5_u64, log.value(1));
        let t2 = queue.schedule(0_u64, 3_u64, log.value(2)); // replaces value 0
        assert!(!queue.cancel(&t0));
        let fired = queue.pop_due(3).expect("value 2 due");
        assert_eq!(fired.value.id, 2);
        drop(fired);
        assert!(!queue.cancel(&t2), "fired generation must not cancel");
        // Asking for the deadline discards the stale entry, dropping value 0.
        assert_eq!(queue.next_deadline(), Some(5));
        let fired = queue.pop_due(5).expect("value 1 due");
        assert_eq!(fired.value.id, 1);
        drop(fired);
    }
    assert_eq!(log.created.load(Ordering::SeqCst), 3);
    assert_eq!(log.dropped.load(Ordering::SeqCst), 3);
    assert!(!log.double_drop.load(Ordering::SeqCst));
}

/// Move-only values held by a dropped queue — live, superseded, and cancelled
/// alike — are dropped exactly once when the queue is dropped.
#[test]
fn move_only_values_dropped_exactly_once_on_queue_drop() {
    let log = Arc::new(DropLog::default());
    {
        let mut queue = TimerQueue::new();
        let mut tokens = Vec::new();
        for key in 0..4_u64 {
            tokens.push(queue.schedule(key, 100, log.value(key)));
        }
        tokens.push(queue.schedule(0, 50, log.value(4))); // supersedes value 0
        assert!(queue.cancel(&tokens[1])); // value 1 cancelled, still in heap
        drop(tokens);
    }
    assert_eq!(log.created.load(Ordering::SeqCst), 5);
    assert_eq!(log.dropped.load(Ordering::SeqCst), 5);
    assert!(!log.double_drop.load(Ordering::SeqCst));
}

/// Signed instant type: extremes of i64 behave identically to u64 edges.
#[test]
fn signed_instant_extremes() {
    let mut queue = TimerQueue::new();
    queue.schedule("min", i64::MIN, 1_u8);
    queue.schedule("zero", 0_i64, 2_u8);
    queue.schedule("max", i64::MAX, 3_u8);
    assert_eq!(queue.next_deadline(), Some(i64::MIN));
    assert_eq!(queue.pop_due(i64::MIN).expect("due").value, 1);
    assert_eq!(queue.pop_due(-1), None, "zero fired early");
    assert_eq!(queue.pop_due(0).expect("due").value, 2);
    assert_eq!(queue.pop_due(i64::MAX - 1), None, "max fired early");
    assert_eq!(queue.pop_due(i64::MAX).expect("due").value, 3);
    assert!(queue.is_empty());
}

/// Repeated fill/drain cycles: after every full drain the buffers shrink, and
/// the queue must keep behaving exactly like a fresh one.
#[test]
fn fill_drain_cycles_behave_like_fresh_queue() {
    let space: Vec<u64> = (0..16).collect();
    let mut rng = Rng(0x00C1_C1E5);
    let mut world: World<u64> = World::new();
    for _cycle in 0..50 {
        for _ in 0..200 {
            let key = rng.below(16) as usize;
            let op = match rng.below(10) {
                0..=5 => Op::Schedule {
                    key,
                    at: rng.below(64),
                },
                6..=7 => Op::CancelCurrent { key },
                8 => Op::CancelStale { key },
                _ => Op::Pop {
                    now: rng.below(64),
                },
            };
            world.apply(&space, op);
        }
        world.drain_check();
    }
}

/// A cloned token is one authority: the first cancel succeeds, the clone's
/// second use fails, and the key stays cancelled between them.
#[test]
fn cloned_token_cancels_exactly_once() {
    let mut queue = TimerQueue::new();
    let token = queue.schedule("k", 10_u64, "v");
    let clone = token.clone();
    assert_eq!(token, clone);
    assert!(queue.cancel(&token));
    assert!(!queue.cancel(&clone), "cloned token cancelled twice");
    assert!(!queue.cancel(&token));
    assert_eq!(queue.pop_due(u64::MAX), None);
}

/// `next_deadline` is idempotent and never consumes a live entry.
#[test]
fn next_deadline_idempotent_and_non_destructive() {
    let mut queue = TimerQueue::new();
    queue.schedule("a", 5_u64, "a");
    queue.schedule("b", 3, "b");
    for _ in 0..3 {
        assert_eq!(queue.next_deadline(), Some(3));
        assert_eq!(queue.len(), 2);
    }
    // Stale entry below the live deadline: idempotent across discard too.
    let old = queue.schedule("b", 1, "b-stale");
    let _new = queue.schedule("b", 7, "b-new");
    assert!(!queue.cancel(&old));
    for _ in 0..3 {
        assert_eq!(queue.next_deadline(), Some(5));
        assert_eq!(queue.len(), 2);
    }
    assert_eq!(queue.pop_due(5).expect("due").value, "a");
    assert_eq!(queue.next_deadline(), Some(7));
}

/// Cancel then reschedule the same key at the same instant: only the new
/// generation exists and fires.
#[test]
fn cancel_then_reschedule_same_instant() {
    let mut queue = TimerQueue::new();
    let t1 = queue.schedule("k", 10_u64, "gen1");
    assert!(queue.cancel(&t1));
    let t2 = queue.schedule("k", 10, "gen2");
    assert!(!queue.cancel(&t1), "cancelled generation revived");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.pop_due(10).expect("due").value, "gen2");
    assert!(!queue.cancel(&t2));
    assert!(queue.is_empty());
}

/// FINDING-001 probe target: tokens carry only `(key, generation)` and no
/// queue identity, so a token minted by one queue cancels a colliding
/// generation in another queue. The correct behavior — "exact authority over
/// one scheduled generation" — requires isolation. See RESEARCH-REPORT.md.
#[test]
#[ignore = "FINDING-001: cross-queue token cancels a foreign generation"]
fn cross_queue_tokens_must_not_cancel() {
    let mut queue_a = TimerQueue::new();
    let mut queue_b = TimerQueue::new();
    let token_a = queue_a.schedule("shared-key", 10_u64, "from-a");
    queue_b.schedule("shared-key", 10_u64, "from-b");
    // The token minted by queue A must have no authority over queue B.
    assert!(
        !queue_b.cancel(&token_a),
        "FINDING-001: queue A's token cancelled queue B's generation"
    );
    assert_eq!(
        queue_b.pop_due(10).expect("queue B timer must survive").value,
        "from-b"
    );
    assert_eq!(
        queue_a.pop_due(10).expect("queue A timer").value,
        "from-a"
    );
}
