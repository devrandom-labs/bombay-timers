//! FINDING-001 fix acceptance battery: queue-branded tokens.
//!
//! The fix gives every `TimerQueue` a private `Arc`-backed brand and every
//! minted `Token` a shared reference to it; `cancel` rejects foreign brands
//! before consulting the generation map. These tests prove the isolation law
//! and pin the costs (one small allocation per queue, +8 bytes per token).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bombay_timers::{TimerQueue, Token};
use timers_tests::DropLog;

/// Many queues with identical keys and matching local generations: every
/// cross-cancel must fail, every own-cancel must succeed.
#[test]
fn many_queues_same_keys_no_cross_authority() {
    const QUEUES: usize = 64;
    let mut queues: Vec<TimerQueue<u64, u64, u64>> =
        (0..QUEUES).map(|_| TimerQueue::new()).collect();
    // Every queue mints generation 1 for key 7 at the same instant.
    let tokens: Vec<Token<u64>> = queues
        .iter_mut()
        .map(|queue| queue.schedule(7, 100, 0xBEEF).unwrap())
        .collect();
    for (owner, queue) in queues.iter_mut().enumerate() {
        for (foreign, token) in tokens.iter().enumerate() {
            if foreign == owner {
                continue;
            }
            assert!(
                !queue.cancel(token),
                "queue {owner} accepted queue {foreign}'s token"
            );
        }
        assert!(queue.cancel(&tokens[owner]), "own token must cancel");
        assert!(queue.is_empty());
    }
}

/// Allocator-reuse probe: drop a queue while retaining its token, then
/// allocate a fresh queue (likely reusing freed memory) and prove the old
/// token still has no authority. The brand's Arc allocation is kept alive by
/// the retained token, so its address can never be reused by a new brand.
#[test]
fn dropped_queue_token_cannot_alias_new_queue() {
    for round in 0..10_000 {
        let token = {
            let mut queue_a = TimerQueue::new();
            let token = queue_a.schedule("k", 10_u64, "from-a").unwrap();
            assert_eq!(queue_a.len(), 1);
            token
            // queue_a dropped here; its heap and map are freed.
        };
        let mut queue_b = TimerQueue::new();
        queue_b.schedule("k", 10_u64, "from-b").unwrap();
        assert!(
            !queue_b.cancel(&token),
            "round {round}: dead queue's token cancelled the new queue"
        );
        assert_eq!(queue_b.pop_due(10).expect("due").value, "from-b");
    }
}

/// A cloned foreign token is exactly as powerless as its original.
#[test]
fn cloned_foreign_tokens_have_no_authority() {
    let mut queue_a = TimerQueue::new();
    let mut queue_b = TimerQueue::new();
    let token = queue_a.schedule("k", 5_u64, "a").unwrap();
    let clone = token.clone();
    queue_b.schedule("k", 5_u64, "b").unwrap();
    assert!(!queue_b.cancel(&token));
    assert!(!queue_b.cancel(&clone));
    // And the clones remain interchangeable at home: one use only.
    assert!(queue_a.cancel(&token));
    assert!(!queue_a.cancel(&clone));
}

/// Foreign tokens stay inert across replacement, churn past the compaction
/// boundary, and a full drain of the foreign queue.
#[test]
fn foreign_tokens_inert_across_replacement_and_compaction() {
    let mut queue_a = TimerQueue::new();
    let foreign = queue_a.schedule("k", 1_u64, "foreign").unwrap();
    let mut queue_b = TimerQueue::new();
    queue_b.schedule("k", 1_u64, 0_u64).unwrap();
    assert!(!queue_b.cancel(&foreign), "foreign before replacement");
    // Replace past two compaction boundaries.
    for round in 1..=2 * 1024_u64 {
        queue_b.schedule("k", round, round).unwrap();
    }
    assert!(!queue_b.cancel(&foreign), "foreign after replacement churn");
    let fired = queue_b.pop_due(u64::MAX).expect("due");
    assert_eq!(fired.value, 2 * 1024);
    assert!(!queue_b.cancel(&foreign), "foreign after full drain");
    assert!(queue_a.cancel(&foreign), "own token still valid at home");
}

/// A rejected foreign cancel must not disturb the foreign queue's value:
/// move-only, drop-counted values prove nothing was consumed or dropped.
#[test]
fn rejected_foreign_cancel_preserves_move_only_value() {
    let log = Arc::new(DropLog::default());
    let mut queue_a = TimerQueue::new();
    let foreign = queue_a
        .schedule(0_u64, 10_u64, "token-carrier")
        .unwrap();
    {
        let mut queue_b = TimerQueue::new();
        queue_b.schedule(0_u64, 10_u64, log.value(1)).unwrap();
        assert!(!queue_b.cancel(&foreign));
        let fired = queue_b.pop_due(10).expect("value must survive");
        assert_eq!(fired.value.id, 1);
    }
    assert_eq!(log.created.load(Ordering::SeqCst), 1);
    assert_eq!(log.dropped.load(Ordering::SeqCst), 1);
    assert!(!log.double_drop.load(Ordering::SeqCst));
    assert!(queue_a.cancel(&foreign));
}

/// Token and queue keep their auto traits: `Send + Sync` exactly when the
/// generic parameters permit, and a token is genuinely usable after crossing
/// a thread boundary.
#[test]
fn token_and_queue_remain_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Token<u64>>();
    assert_send_sync::<TimerQueue<u64, u64, u64>>();

    let mut queue = TimerQueue::new();
    let token = queue.schedule("k", 9_u64, "threaded").unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            assert_eq!(token.key(), &"k");
            token
        });
        let returned = handle.join().expect("scoped thread panicked");
        assert!(queue.cancel(&returned), "token must work after crossing threads");
    });
}

/// Cost pins: one token carries exactly one extra pointer over the old
/// `(key, generation)` pair.
#[test]
fn token_size_cost_is_one_pointer() {
    assert_eq!(size_of::<Token<u64>>(), 24);
    assert_eq!(size_of::<Token<u8>>(), 24);
}
