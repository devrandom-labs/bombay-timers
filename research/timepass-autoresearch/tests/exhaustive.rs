//! Exhaustive small-state exploration: enumerate every reachable canonical
//! behavioral state over tiny key/instant domains, extend each by every
//! possible op, verify observables after every step, and fully drain-verify
//! every visited state. A transposition table over `World::canonical` prunes
//! revisits, so the exploration is complete for each domain (the depth cap is
//! a termination guard, not a coverage bound).

use std::collections::HashSet;

use timepass_autoresearch::{Op, World};

/// One small exploration domain.
struct Domain {
    keys: Vec<u64>,
    ats: &'static [u64],
    nows: &'static [u64],
}

impl Domain {
    fn ops(&self) -> Vec<Op> {
        let mut ops = Vec::new();
        for key in 0..self.keys.len() {
            for &at in self.ats {
                ops.push(Op::Schedule { key, at });
            }
            ops.push(Op::CancelCurrent { key });
            ops.push(Op::CancelStale { key });
        }
        for &now in self.nows {
            ops.push(Op::Pop { now });
        }
        ops
    }
}

#[derive(Default)]
struct Stats {
    states: usize,
    edges: usize,
    max_depth: usize,
}

type Canon = (Vec<(usize, u64)>, Vec<usize>);

fn explore(domain: &Domain, max_depth: usize) -> Stats {
    let mut stats = Stats::default();
    let mut visited: HashSet<Canon> = HashSet::new();
    let mut path: Vec<Op> = Vec::new();
    walk(domain, max_depth, &mut path, &mut visited, &mut stats);
    stats
}

fn walk(
    domain: &Domain,
    max_depth: usize,
    path: &mut Vec<Op>,
    visited: &mut HashSet<Canon>,
    stats: &mut Stats,
) {
    // Replay the path from scratch: the queue is not Clone, and replaying
    // also re-verifies every intermediate observable on every visit.
    let mut world = World::new();
    for &op in path.iter() {
        world.apply(&domain.keys, op);
    }
    if !visited.insert(world.canonical(&domain.keys)) {
        return;
    }
    stats.states += 1;
    stats.max_depth = stats.max_depth.max(path.len());
    if path.len() < max_depth {
        for op in domain.ops() {
            path.push(op);
            stats.edges += 1;
            walk(domain, max_depth, path, visited, stats);
            path.pop();
        }
    }
    // Full destructive drain verification of this exact state.
    world.drain_check();
}

/// Termination guard; the transposition table bounds the real work.
const MAX_DEPTH: usize = 400;

#[test]
fn exhaustive_two_keys_three_instants() {
    let domain = Domain {
        keys: vec![0, 1],
        ats: &[0, 1, 2],
        nows: &[0, 1, 2],
    };
    let stats = explore(&domain, MAX_DEPTH);
    println!(
        "two_keys_three_instants: states={} edges={} max_depth={}",
        stats.states, stats.edges, stats.max_depth
    );
    assert!(stats.states >= 76, "exploration visited fewer states than the known-complete space");
}

#[test]
fn exhaustive_two_keys_extreme_instants() {
    let domain = Domain {
        keys: vec![0, 1],
        ats: &[0, u64::MAX - 1, u64::MAX],
        nows: &[0, u64::MAX - 1, u64::MAX],
    };
    let stats = explore(&domain, MAX_DEPTH);
    println!(
        "two_keys_extreme_instants: states={} edges={} max_depth={}",
        stats.states, stats.edges, stats.max_depth
    );
    assert!(stats.states >= 76, "exploration visited fewer states than the known-complete space");
}

#[cfg(not(miri))]
#[test]
fn exhaustive_three_keys_two_instants() {
    let domain = Domain {
        keys: vec![0, 1, 2],
        ats: &[0, 1],
        nows: &[0, 1],
    };
    let stats = explore(&domain, MAX_DEPTH);
    println!(
        "three_keys_two_instants: states={} edges={} max_depth={}",
        stats.states, stats.edges, stats.max_depth
    );
    assert!(stats.states >= 392, "exploration visited fewer states than the known-complete space");
}

#[cfg(not(miri))]
#[test]
fn exhaustive_three_keys_three_instants() {
    let domain = Domain {
        keys: vec![0, 1, 2],
        ats: &[0, 1, 2],
        nows: &[0, 1, 2],
    };
    let stats = explore(&domain, MAX_DEPTH);
    println!(
        "three_keys_three_instants: states={} edges={} max_depth={}",
        stats.states, stats.edges, stats.max_depth
    );
    assert!(
        stats.states >= 392,
        "exploration visited fewer states than the smaller 3-key domain"
    );
}

#[cfg(not(miri))]
#[test]
fn exhaustive_three_keys_dense_cancel() {
    // One instant only: every schedule ties, so replacement, cancellation,
    // and firing order interact maximally.
    let domain = Domain {
        keys: vec![0, 1, 2],
        ats: &[7],
        nows: &[0, 7],
    };
    let stats = explore(&domain, MAX_DEPTH);
    println!(
        "three_keys_dense_cancel: states={} edges={} max_depth={}",
        stats.states, stats.edges, stats.max_depth
    );
    assert!(stats.states >= 128, "exploration visited fewer states than the known-complete space");
}
