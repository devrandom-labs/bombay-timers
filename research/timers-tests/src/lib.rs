//! Adversarial test-only campaign infrastructure for the `bombay-timers` scheduler.
//!
//! Everything in this crate is test infrastructure: an independent reference
//! model ([`World`]), adversarial key/value types, and a byte-stream op
//! interpreter shared by the property tests, the deterministic campaign, and
//! the coverage-guided fuzz target. Production code is never modified.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bombay_timers::{TimerQueue, Token};

/// One adversarial operation against the scheduler under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Op {
    /// Schedule (or replace) `key` at instant `at`. The value is the schedule
    /// ordinal, so value integrity is verified by construction.
    Schedule { key: usize, at: u64 },
    /// Cancel with the newest outstanding token for `key`, if one exists.
    CancelCurrent { key: usize },
    /// Cancel with the newest retired (stale) token for `key`, if one exists.
    CancelStale { key: usize },
    /// Pop one timer due at or before `now`.
    Pop { now: u64 },
}

/// Independent reference model plus the scheduler under test.
///
/// The model is deliberately naive: a `BTreeMap` of live schedules and a
/// `BTreeSet` ordered by `(at, seq)` — the documented equal-deadline firing
/// order (one sequence step per schedule, matching the queue's `sequence`
/// tiebreak). It shares no code with the implementation under test.
pub struct World<K> {
    queue: TimerQueue<u64, K, u64>,
    /// key -> (at, seq) for every live schedule.
    live: BTreeMap<K, (u64, u64)>,
    /// (at, seq, key) in firing order.
    order: BTreeSet<(u64, u64, K)>,
    /// Next schedule ordinal; doubles as the value and the model sequence.
    seq: u64,
    /// Newest outstanding token per live key.
    current: HashMap<K, Token<K>>,
    /// Retired (stale) tokens per key; cancelling with any of these must fail.
    stale: HashMap<K, Vec<Token<K>>>,
}

impl<K> Default for World<K>
where
    K: Clone + Eq + Hash + Ord + Debug,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K> World<K>
where
    K: Clone + Eq + Hash + Ord + Debug,
{
    pub fn new() -> Self {
        Self {
            queue: TimerQueue::new(),
            live: BTreeMap::new(),
            order: BTreeSet::new(),
            seq: 0,
            current: HashMap::new(),
            stale: HashMap::new(),
        }
    }

    /// Apply one op, then verify the observable state against the model.
    pub fn apply(&mut self, space: &[K], op: Op) {
        assert!(!space.is_empty(), "key space must be non-empty");
        match op {
            Op::Schedule { key, at } => {
                let key = space[key % space.len()].clone();
                self.schedule(key, at);
            }
            Op::CancelCurrent { key } => {
                let key = space[key % space.len()].clone();
                self.cancel_current(&key);
            }
            Op::CancelStale { key } => {
                let key = space[key % space.len()].clone();
                self.cancel_stale(&key);
            }
            Op::Pop { now } => self.pop(now),
        }
        self.check_observations();
    }

    fn schedule(&mut self, key: K, at: u64) {
        let seq = self.seq;
        self.seq += 1;
        let token = self.queue.schedule(key.clone(), at, seq).unwrap();
        assert_eq!(token.key(), &key, "token names the wrong key");
        if let Some((old_at, old_seq)) = self.live.insert(key.clone(), (at, seq)) {
            assert!(
                self.order.remove(&(old_at, old_seq, key.clone())),
                "model inconsistency: replaced entry missing from order set"
            );
            let retired = self
                .current
                .insert(key.clone(), token)
                .expect("model inconsistency: live key without a token");
            self.stale.entry(key.clone()).or_default().push(retired);
        } else {
            self.current.insert(key.clone(), token);
        }
        self.order.insert((at, seq, key));
    }

    fn cancel_current(&mut self, key: &K) {
        let expected = self.live.contains_key(key);
        let Some(token) = self.current.get(key) else {
            assert!(
                !expected,
                "model inconsistency: live key {key:?} has no current token"
            );
            return;
        };
        let got = self.queue.cancel(token);
        assert_eq!(
            got, expected,
            "cancel with the current token for {key:?} returned {got}, expected {expected}"
        );
        if got {
            let (at, seq) = self.live.remove(key).expect("checked above");
            self.order.remove(&(at, seq, key.clone()));
            let retired = self.current.remove(key).expect("checked above");
            self.stale.entry(key.clone()).or_default().push(retired);
        }
    }

    fn cancel_stale(&mut self, key: &K) {
        if let Some(retired) = self.stale.get(key) {
            // Probe the oldest and newest retired tokens: the oldest is where
            // any generation-aliasing defect would surface first.
            for token in retired.first().into_iter().chain(retired.last()) {
                assert!(
                    !self.queue.cancel(token),
                    "stale token for {key:?} cancelled a live generation"
                );
            }
        }
    }

    fn pop(&mut self, now: u64) {
        let expected = self
            .order
            .first()
            .filter(|(at, _, _)| *at <= now)
            .map(|(at, seq, key)| (*at, *seq, key.clone()));
        let got = self.queue.pop_due(now);
        match (got, expected) {
            (Some(expired), Some((at, seq, key))) => {
                assert_eq!(
                    (expired.at, expired.value, &expired.key),
                    (at, seq, &key),
                    "pop_due({now}) fired the wrong timer"
                );
                self.live.remove(&key);
                self.order.remove(&(at, seq, key.clone()));
                let retired = self
                    .current
                    .remove(&key)
                    .expect("model inconsistency: fired key had no token");
                self.stale.entry(key).or_default().push(retired);
            }
            (None, None) => {}
            (got, expected) => {
                panic!("pop_due({now}) divergence: got {got:?}, model expected {expected:?}");
            }
        }
    }

    /// Verify every non-destructive observable: live count and next deadline.
    pub fn check_observations(&mut self) {
        assert_eq!(
            self.queue.len(),
            self.live.len(),
            "len divergence: queue reports {}, model holds {}",
            self.queue.len(),
            self.live.len()
        );
        assert_eq!(
            self.queue.is_empty(),
            self.live.is_empty(),
            "is_empty divergence"
        );
        let expected = self.order.first().map(|&(at, _, _)| at);
        assert_eq!(
            self.queue.next_deadline(),
            expected,
            "next_deadline divergence"
        );
    }

    /// Destructive endgame: drain the queue completely and verify the full
    /// firing order, value integrity, and post-drain state.
    pub fn drain_check(&mut self) {
        while let Some(expired) = self.queue.pop_due(u64::MAX) {
            let Some((at, seq, key)) = self.order.pop_first() else {
                panic!("queue fired more timers than the model holds (extra: {expired:?})");
            };
            assert_eq!(
                (expired.at, expired.value, &expired.key),
                (at, seq, &key),
                "drain firing-order divergence"
            );
            self.live.remove(&key);
        }
        assert!(
            self.order.is_empty(),
            "model holds timers the queue never fired"
        );
        assert!(self.live.is_empty());
        assert_eq!(self.queue.len(), 0, "len nonzero after full drain");
        assert!(self.queue.is_empty(), "queue not empty after full drain");
        assert_eq!(
            self.queue.next_deadline(),
            None,
            "deadline reported after full drain"
        );
        // Every outstanding token is now retired; all cancels must fail.
        let retired: Vec<Token<K>> = self
            .current
            .drain()
            .map(|(_, token)| token)
            .chain(
                self.stale
                    .values_mut()
                    .flat_map(|tokens| tokens.drain(..)),
            )
            .collect();
        for token in &retired {
            assert!(
                !self.queue.cancel(token),
                "cancel succeeded after the generation fired"
            );
        }
    }

    /// Canonical behavioral state: the live schedules in firing order plus
    /// the keys holding retired tokens. Two histories with the same canonical
    /// state are observationally indistinguishable, so explorers may prune.
    pub fn canonical(&self, space: &[K]) -> (Vec<(usize, u64)>, Vec<usize>) {
        let firing = self
            .order
            .iter()
            .map(|(_, _, key)| {
                let index = space
                    .iter()
                    .position(|candidate| candidate == key)
                    .expect("scheduled key outside the space");
                (index, self.live[key].0)
            })
            .collect();
        let mut staled: Vec<usize> = self
            .stale
            .iter()
            .filter(|(_, tokens)| !tokens.is_empty())
            .filter_map(|(key, _)| space.iter().position(|candidate| candidate == key))
            .collect();
        staled.sort_unstable();
        (firing, staled)
    }
}

/// A key whose hash is constant: every key collides into a single probe
/// chain, adversarially exercising the generation map's collision handling,
/// backward-shift deletion, and growth under maximal clustering.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct CollideKey(pub u64);

impl Hash for CollideKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u8(0);
    }
}

/// Shared drop ledger: counts creations and drops and detects double drops.
#[derive(Debug, Default)]
pub struct DropLog {
    pub created: AtomicUsize,
    pub dropped: AtomicUsize,
    pub double_drop: AtomicBool,
    live: Mutex<HashSet<u64>>,
}

impl DropLog {
    /// Mint a move-only tracked value.
    pub fn value(self: &Arc<Self>, id: u64) -> DropValue {
        self.created.fetch_add(1, Ordering::SeqCst);
        self.live.lock().expect("drop log poisoned").insert(id);
        DropValue {
            id,
            log: Arc::clone(self),
        }
    }

    fn record_drop(&self, id: u64) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
        if !self.live.lock().expect("drop log poisoned").remove(&id) {
            self.double_drop.store(true, Ordering::SeqCst);
        }
    }
}

/// A move-only value that reports every drop to its [`DropLog`].
#[derive(Debug)]
pub struct DropValue {
    pub id: u64,
    log: Arc<DropLog>,
}

impl Drop for DropValue {
    fn drop(&mut self) {
        self.log.record_drop(self.id);
    }
}

/// Deterministic xorshift64* generator for reproducible campaigns.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Interpret `bytes` as an op stream and check every step against the model.
///
/// Grammar: `[tag, key, payload…]` repeating; the tag is `byte % 4`
/// (schedule / cancel-current / cancel-stale / pop). Schedule and pop consume
/// an 8-byte little-endian payload (`at` / `now`); a truncated tail ends the
/// stream. In narrow mode, payloads are masked to 6 bits to force deadline
/// collisions; wide mode exercises extreme instants up to `u64::MAX`.
///
/// This is the exact entrypoint the coverage-guided fuzz target drives, so a
/// fuzz crash input reproduces through this function directly.
pub fn run_bytes<K>(space: &[K], bytes: &[u8], narrow: bool)
where
    K: Clone + Eq + Hash + Ord + Debug,
{
    let mut world = World::new();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        let tag = bytes[i] % 4;
        let key = usize::from(bytes[i + 1]);
        match tag {
            0 => {
                if i + 10 > bytes.len() {
                    break;
                }
                let mut at = u64::from_le_bytes(
                    bytes[i + 2..i + 10].try_into().expect("8-byte payload"),
                );
                if narrow {
                    at &= 0x3F;
                }
                world.apply(space, Op::Schedule { key, at });
                i += 10;
            }
            1 => {
                world.apply(space, Op::CancelCurrent { key });
                i += 2;
            }
            2 => {
                world.apply(space, Op::CancelStale { key });
                i += 2;
            }
            _ => {
                if i + 10 > bytes.len() {
                    break;
                }
                let mut now = u64::from_le_bytes(
                    bytes[i + 2..i + 10].try_into().expect("8-byte payload"),
                );
                if narrow {
                    now &= 0x3F;
                }
                world.apply(space, Op::Pop { now });
                i += 10;
            }
        }
    }
    world.drain_check();
}

/// The key space used by the byte-stream campaign and the fuzz target.
pub fn campaign_space() -> Vec<u64> {
    (0..8).collect()
}

/// Run `streams` deterministic pseudo-random op streams from `seed` through
/// [`run_bytes`], alternating narrow (collision-dense) and wide (extreme
/// instant) payload modes, over both the `u64` and the colliding key spaces.
/// Returns the number of streams executed.
pub fn byte_campaign(streams: u64, max_len: usize, seed: u64) -> u64 {
    let u64_space = campaign_space();
    let collide_space: Vec<CollideKey> = (0..8).map(CollideKey).collect();
    let mut rng = Rng(seed | 1);
    for stream in 0..streams {
        let len = 2 + rng.below((max_len - 2) as u64) as usize;
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            bytes.extend_from_slice(&rng.next_u64().to_le_bytes());
        }
        bytes.truncate(len);
        let narrow = stream % 2 == 0;
        if stream % 4 == 3 {
            run_bytes(&collide_space, &bytes, narrow);
        } else {
            run_bytes(&u64_space, &bytes, narrow);
        }
    }
    streams
}
