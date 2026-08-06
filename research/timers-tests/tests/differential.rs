//! Differential property campaign: randomized op streams checked against the
//! independent `World` model. Skipped under Miri — the deterministic suites
//! (exhaustive, adversarial, byte campaign) carry the Miri evidence, and the
//! property suites carry the randomized native evidence.

#![cfg(not(miri))]

use proptest::prelude::*;
use timers_tests::{CollideKey, Op, World};

/// Instant extremes plus zero: never-early and equal-deadline behavior at the
/// edges of the instant domain.
const EXTREMES: [u64; 4] = [0, 1, u64::MAX - 1, u64::MAX];

fn at_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        8 => 0..16u64,
        2 => prop::sample::select(EXTREMES.to_vec()),
        1 => any::<u64>(),
    ]
}

/// Weighted op strategy. Heavier schedule weight builds stale-entry churn;
/// heavier cancel weight stresses generation matching and map deletion.
fn op_strategy(
    keys: usize,
    schedule_w: u32,
    cancel_w: u32,
    stale_w: u32,
    pop_w: u32,
) -> impl Strategy<Value = Op> {
    prop_oneof![
        schedule_w => (0..keys, at_strategy()).prop_map(|(key, at)| Op::Schedule { key, at }),
        cancel_w => (0..keys).prop_map(|key| Op::CancelCurrent { key }),
        stale_w => (0..keys).prop_map(|key| Op::CancelStale { key }),
        pop_w => at_strategy().prop_map(|now| Op::Pop { now }),
    ]
}

fn run<K>(space: &[K], ops: &[Op])
where
    K: Clone + Eq + std::hash::Hash + Ord + std::fmt::Debug,
{
    let mut world = World::new();
    for &op in ops {
        world.apply(space, op);
    }
    world.drain_check();
}

fn u64_space(keys: usize) -> Vec<u64> {
    (0..keys as u64).collect()
}

fn collide_space(keys: usize) -> Vec<CollideKey> {
    (0..keys as u64).map(CollideKey).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Dense small domain: four keys and mostly-tied deadlines maximize
    /// equal-deadline ordering, replacement, and cancellation interactions.
    #[test]
    fn differential_small_domain(ops in prop::collection::vec(op_strategy(4, 6, 2, 1, 3), 1..=200)) {
        run(&u64_space(4), &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Wide domain: 64 keys and arbitrary u64 instants, including extremes.
    #[test]
    fn differential_wide_domain(ops in prop::collection::vec(op_strategy(64, 6, 2, 1, 3), 1..=400)) {
        run(&u64_space(64), &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Replace-heavy churn: long streams dominated by rescheduling build deep
    /// stale-entry piles and cross the 1024-schedule compaction boundary
    /// repeatedly, stressing heap/map divergence and compaction correctness.
    #[test]
    fn differential_replace_heavy_churn(ops in prop::collection::vec(op_strategy(8, 16, 1, 1, 2), 1500..=2500)) {
        run(&u64_space(8), &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Cancel-heavy: cancellations and stale-token replays against a churning
    /// live set, stressing generation-exact cancellation and map deletion.
    #[test]
    fn differential_cancel_heavy(ops in prop::collection::vec(op_strategy(8, 4, 6, 2, 3), 1..=600)) {
        run(&u64_space(8), &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Degenerate hashing: every key collides into one probe chain,
    /// adversarially exercising insertion, backward-shift deletion, and
    /// growth under maximal clustering.
    #[test]
    fn differential_colliding_keys(ops in prop::collection::vec(op_strategy(16, 6, 2, 2, 3), 1..=300)) {
        run(&collide_space(16), &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Extreme-tie domain: instants only at the four domain edges, so nearly
    /// every deadline ties and the `(at, sequence)` order is decided by
    /// sequence alone at 0 and at u64::MAX.
    #[test]
    fn differential_extreme_ties(
        ops in prop::collection::vec(
            prop_oneof![
                6 => (0..6usize, prop::sample::select(EXTREMES.to_vec()))
                    .prop_map(|(key, at)| Op::Schedule { key, at }),
                2 => (0..6usize).prop_map(|key| Op::CancelCurrent { key }),
                1 => (0..6usize).prop_map(|key| Op::CancelStale { key }),
                3 => prop::sample::select(EXTREMES.to_vec()).prop_map(|now| Op::Pop { now }),
            ],
            1..=300,
        )
    ) {
        run(&u64_space(6), &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Pop-heavy: drains dominate, so the queue cycles through
    /// fill/drain/refill constantly, stressing the post-drain shrink path and
    /// reuse of a fully drained queue.
    #[test]
    fn differential_pop_heavy(ops in prop::collection::vec(op_strategy(8, 3, 1, 1, 8), 1..=400)) {
        run(&u64_space(8), &ops);
    }
}
