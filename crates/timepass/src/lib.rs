//! A keyed, generation-safe monotonic timer queue.

use core::cmp::{Ordering, Reverse};
use core::hash::Hash;
use rustc_hash::FxHashMap;
use std::collections::BinaryHeap;
use std::mem;

/// Schedule interval between heap-compaction checks; a power of two.
const COMPACT_INTERVAL: u64 = 1024;

/// Exact authority over one scheduled generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<K> {
    key: K,
    generation: u64,
}

impl<K> Token<K> {
    /// Inspect the scheduled key.
    #[must_use]
    pub const fn key(&self) -> &K {
        &self.key
    }
}

struct Entry<I, K, V> {
    at: I,
    sequence: u64,
    generation: u64,
    key: K,
    value: V,
}

impl<I: Ord, K, V> PartialEq for Entry<I, K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.sequence == other.sequence
    }
}
impl<I: Ord, K, V> Eq for Entry<I, K, V> {}
impl<I: Ord, K, V> PartialOrd for Entry<I, K, V> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<I: Ord, K, V> Ord for Entry<I, K, V> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at
            .cmp(&other.at)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

/// An expiration emitted by the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expired<I, K, V> {
    /// Requested deadline.
    pub at: I,
    /// Scheduled key.
    pub key: K,
    /// Scheduled value.
    pub value: V,
}

/// Safe reference scheduler using a binary heap and generation index.
pub struct TimerQueue<I, K, V> {
    heap: BinaryHeap<Reverse<Entry<I, K, V>>>,
    current: FxHashMap<K, u64>,
    nextpopped_generation: u64,
    next_sequence: u64,
}

impl<I, K, V> Default for TimerQueue<I, K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I, K, V> TimerQueue<I, K, V> {
    /// Construct an empty timer queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            current: FxHashMap::default(),
            nextpopped_generation: 1,
            next_sequence: 0,
        }
    }

    /// Number of current keyed schedules, excluding stale heap entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.current.len()
    }

    /// Whether no key is currently scheduled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.current.is_empty()
    }
}

impl<I, K, V> TimerQueue<I, K, V>
where
    I: Copy + Ord,
    K: Clone + Eq + Hash,
{
    /// Schedule or replace one keyed value.
    ///
    /// # Panics
    /// Panics if the process exhausts all timer generations or insertion
    /// sequence values.
    pub fn schedule(&mut self, key: K, at: I, value: V) -> Token<K> {
        let generation = self.nextpopped_generation;
        self.nextpopped_generation = self
            .nextpopped_generation
            .checked_add(1)
            .expect("timer generation exhausted");
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("timer sequence exhausted");
        self.current.insert(key.clone(), generation);
        self.heap.push(Reverse(Entry {
            at,
            sequence,
            generation,
            key: key.clone(),
            value,
        }));
        // Bound the heap while the queue is growing: replace-heavy phases
        // accumulate stale generations that would otherwise peak at the full
        // schedule count. The check is amortized over `COMPACT_INTERVAL`
        // schedules so the fresh-key path pays one AND+branch; the peak bound
        // then slackens by at most `COMPACT_INTERVAL` entries.
        if self.next_sequence & (COMPACT_INTERVAL - 1) == 0 {
            self.compact();
        }
        Token { key, generation }
    }

    /// Cancel exactly the generation named by `token`.
    pub fn cancel(&mut self, token: &Token<K>) -> bool {
        if self.current.get(&token.key) == Some(&token.generation) {
            self.current.remove(&token.key);
            true
        } else {
            false
        }
    }

    /// Return the earliest current deadline, removing stale heap entries.
    pub fn next_deadline(&mut self) -> Option<I> {
        self.discard_stale();
        self.heap.peek().map(|entry| entry.0.at)
    }

    /// Pop one current timer due at or before `now`.
    pub fn pop_due(&mut self, now: I) -> Option<Expired<I, K, V>> {
        self.discard_stale();
        if self.heap.peek().is_none_or(|entry| entry.0.at > now) {
            return None;
        }
        let Reverse(entry) = self.heap.pop()?;
        self.current.remove(&entry.key);
        Some(Expired {
            at: entry.at,
            key: entry.key,
            value: entry.value,
        })
    }

    /// Remove stale heap entries once they outnumber the live schedules and
    /// release oversized buffers after a full drain.
    ///
    /// Lazy top-of-heap discard bounds nothing: replace-heavy workloads leave
    /// every superseded generation in the heap until it happens to surface.
    /// Once the heap exceeds twice the live count, rebuild it from the
    /// generation index in O(n) and shrink the buffer, bounding memory to
    /// ~2x live entries. The retained entries are exactly those the discard
    /// loop would eventually pop, so ordering and firing semantics are
    /// unchanged.
    ///
    /// When every key has been drained or cancelled, both containers still
    /// pin the peak capacity; release it so a long-lived queue does not
    /// retain a workload's peak footprint.
    fn compact(&mut self) {
        // Compaction triggers when stale entries outnumber live ones, i.e.
        // `heap > 2 * live`. Every live key has exactly one heap entry, so
        // `checked_sub` cannot underflow; on the impossible underflow path,
        // skip compaction (the safe direction).
        let Some(stale) = self.heap.len().checked_sub(self.current.len()) else {
            return;
        };
        if stale > self.current.len() {
            let mut vec = mem::take(&mut self.heap).into_vec();
            vec.retain(|Reverse(entry)| self.current.get(&entry.key) == Some(&entry.generation));
            vec.shrink_to_fit();
            self.heap = BinaryHeap::from(vec);
        }
        if self.current.is_empty() {
            self.current.shrink_to_fit();
            self.heap.shrink_to_fit();
        }
    }

    fn discard_stale(&mut self) {
        self.compact();
        while self
            .heap
            .peek()
            .is_some_and(|entry| self.current.get(&entry.0.key) != Some(&entry.0.generation))
        {
            self.heap.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        reason = "tests deliberately truncate u64 RNG outputs to small key/now ranges"
    )]
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// Deterministic xorshift64* generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    /// Differential test: the scheduler against a BTreeSet/BTreeMap reference
    /// model over a seeded stream of schedules, replaces, cancels, and pops.
    /// The model orders by `(at, generation)`, which is the queue's
    /// equal-deadline schedule order (generation increases exactly once per
    /// schedule).
    #[test]
    fn differential_against_reference_model() {
        const KEYS: usize = 64;
        let mut rng = Rng(0xDEAD_BEEF_1234);
        let mut queue = TimerQueue::new();
        let mut tokens: Vec<Option<Token<u64>>> = vec![None; KEYS];
        // Reference model: per-key live schedule and a set ordered by deadline.
        let mut live: BTreeMap<u64, (u64, u64, u64)> = BTreeMap::new(); // key -> (at, gen, value)
        let mut by_deadline: BTreeSet<(u64, u64, u64)> = BTreeSet::new(); // (at, gen, key)
        let mut modelpopped_gen = 0_u64;

        for _ in 0..50_000 {
            let key = (rng.next() % KEYS as u64) as usize;
            let roll = rng.next() % 10;
            match roll {
                0..=5 => {
                    let at = rng.next() % 100;
                    let value = rng.next();
                    let token = queue.schedule(key as u64, at, value);
                    tokens[key] = Some(token);
                    modelpopped_gen += 1;
                    if let Some(old) = live.insert(key as u64, (at, modelpopped_gen, value)) {
                        by_deadline.remove(&(old.0, old.1, key as u64));
                    }
                    by_deadline.insert((at, modelpopped_gen, key as u64));
                }
                6..=7 => {
                    if let Some(token) = tokens[key].as_ref() {
                        let got = queue.cancel(token);
                        let expected = live
                            .get(&(key as u64))
                            .is_some_and(|&(_, g, _)| g == token.generation);
                        assert_eq!(got, expected, "cancel mismatch for key {key}");
                        if got {
                            tokens[key] = None;
                            if let Some(old) = live.remove(&(key as u64)) {
                                by_deadline.remove(&(old.0, old.1, key as u64));
                            }
                        }
                    }
                }
                8..=9 => {
                    let now = rng.next() % 100;
                    let got = queue.pop_due(now);
                    let expected = by_deadline.first().copied().filter(|&(at, _, _)| at <= now);
                    match (got, expected) {
                        (Some(expired), Some((at, popped_gen, k))) => {
                            let (_, _, v) = live[&k];
                            assert_eq!((expired.at, expired.key, expired.value), (at, k, v));
                            by_deadline.remove(&(at, popped_gen, k));
                            live.remove(&k);
                        }
                        (None, None) => {}
                        (got, expected) => {
                            panic!("pop_due mismatch: got {got:?}, expected {expected:?}")
                        }
                    }
                    let nd = queue.next_deadline();
                    assert_eq!(
                        nd,
                        by_deadline.first().map(|&(at, _, _)| at),
                        "next_deadline mismatch after pop at now={now}"
                    );
                }
                _ => unreachable!(),
            }
            assert_eq!(queue.len(), live.len(), "live-count mismatch after op");
        }

        // Final full drain must empty both in the same order.
        while let Some(expired) = queue.pop_due(u64::MAX) {
            let (at, _popped_gen, k) = by_deadline.pop_first().expect("model exhausted early");
            assert_eq!(expired.at, at);
            assert_eq!(expired.key, k);
        }
        assert!(by_deadline.is_empty(), "model should be empty after drain");
        assert!(queue.is_empty());
    }

    /// `&str` keys exercise the non-integer hash path of the generation map.
    #[test]
    fn str_keys_schedule_replace_cancel_pop() {
        let mut queue = TimerQueue::new();
        let old = queue.schedule("actor", 10_u64, "old");
        let new = queue.schedule("actor", 20, "new");
        assert!(!queue.cancel(&old));
        assert_eq!(queue.next_deadline(), Some(20));
        assert_eq!(queue.pop_due(19), None);
        assert_eq!(queue.pop_due(20).unwrap().value, "new");
        assert!(!queue.cancel(&new));
    }
}
