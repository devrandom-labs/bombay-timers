//! A keyed, generation-safe monotonic timer queue.

use core::cmp::{Ordering, Reverse};
use core::fmt;
use core::hash::{Hash, Hasher};
use rustc_hash::FxHasher;
use std::collections::BinaryHeap;
use std::mem;
use std::sync::Arc;

/// Schedule interval between heap-compaction checks; a power of two.
const COMPACT_INTERVAL: u64 = 1024;

/// Unforgeable queue identity. One brand is allocated per [`TimerQueue`] and
/// shared by every token the queue mints; because tokens hold an `Arc` to
/// their brand, the brand allocation outlives its queue whenever tokens do,
/// so a later queue can never occupy the same address and alias its
/// authority. The type is private and the API exposes no way to construct or
/// compare brands, so a brand cannot be forged.
struct QueueBrand;

/// Exact authority over one scheduled generation.
///
/// Authority is scoped to the issuing queue: a token minted by one
/// [`TimerQueue`] can never cancel a schedule in another queue, even when key
/// and generation number coincide.
#[derive(Clone)]
pub struct Token<K> {
    brand: Arc<QueueBrand>,
    key: K,
    generation: u64,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the brand is deliberately opaque: it identifies the issuing queue, not the schedule"
)]
impl<K: fmt::Debug> fmt::Debug for Token<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("key", &self.key)
            .field("generation", &self.generation)
            .finish()
    }
}

impl<K: PartialEq> PartialEq for Token<K> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.brand, &other.brand)
            && self.key == other.key
            && self.generation == other.generation
    }
}
impl<K: Eq> Eq for Token<K> {}

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
    #[inline]
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

/// A rejected scheduling request, retaining ownership of its complete input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError<I, K, V> {
    /// No further timer generation can be represented.
    GenerationExhausted { key: K, at: I, value: V },
    /// No further insertion sequence can be represented.
    SequenceExhausted { key: K, at: I, value: V },
}

/// Minimal open-addressing map: key -> generation.
///
/// Linear probing with tombstones over power-of-two capacity, `FxHash`. The
/// scheduler only needs `insert`/`get`/`remove`/`len`/`shrink_to_fit` — never
/// iteration — so a purpose-built probe table beats a Swiss-table `HashMap` on
/// the hot path: one state-byte load per probe instead of a 16-byte SIMD
/// control-group scan, at the same `FxHash` cost.
struct GenMap<K> {
    keys: Vec<Option<K>>,
    gens: Vec<u64>,
    /// Occupied slots.
    len: usize,
    /// `(capacity() * 3) / 4`, cached so the per-insert growth check is a
    /// compare instead of a multiply.
    grow_threshold: usize,
}

impl<K> Default for GenMap<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> GenMap<K> {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            gens: Vec::new(),
            len: 0,
            grow_threshold: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn capacity(&self) -> usize {
        self.keys.len()
    }

    fn hash(key: &K) -> u64
    where
        K: Hash,
    {
        let mut hasher = FxHasher::default();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Probe start for `key` under `mask` (capacity minus one, a power of two
    /// minus one). Truncating the hash to `usize` is deliberate: only the low
    /// bits select the table slot.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "hash-to-index truncation is inherent to table sizing"
    )]
    fn index(key: &K, mask: usize) -> usize
    where
        K: Hash,
    {
        Self::hash(key) as usize & mask
    }

    /// Rebuild the table at exactly `capacity` (a power of two, zero allowed),
    /// reinserting the occupied entries.
    fn rebuild(&mut self, capacity: usize)
    where
        K: Hash,
    {
        let mut keys: Vec<Option<K>> = Vec::with_capacity(capacity);
        keys.resize_with(capacity, || None);
        let mut gens = vec![0_u64; capacity];
        for (i, slot) in self.keys.drain(..).enumerate() {
            if let Some(key) = slot {
                let mask = capacity - 1;
                let mut j = Self::index(&key, mask);
                while keys[j].is_some() {
                    j = (j + 1) & mask;
                }
                keys[j] = Some(key);
                gens[j] = self.gens[i];
            }
        }
        self.keys = keys;
        self.gens = gens;
        self.grow_threshold = (self.capacity() * 3) / 4;
    }

    fn grow(&mut self)
    where
        K: Hash,
    {
        let new_cap = if self.capacity() == 0 {
            4
        } else {
            self.capacity() * 2
        };
        self.rebuild(new_cap);
    }

    fn insert(&mut self, key: K, generation: u64) -> Option<u64>
    where
        K: Eq + Hash,
    {
        if self.len >= self.grow_threshold {
            self.grow();
        }
        let mask = self.capacity() - 1;
        let mut i = Self::index(&key, mask);
        loop {
            match &self.keys[i] {
                None => {
                    self.keys[i] = Some(key);
                    self.gens[i] = generation;
                    self.len += 1;
                    return None;
                }
                Some(k) if k == &key => {
                    return Some(mem::replace(&mut self.gens[i], generation));
                }
                Some(_) => {}
            }
            i = (i + 1) & mask;
        }
    }

    fn get(&self, key: &K) -> Option<&u64>
    where
        K: Eq + Hash,
    {
        if self.capacity() == 0 {
            return None;
        }
        let mask = self.capacity() - 1;
        let mut i = Self::index(key, mask);
        loop {
            match self.keys[i] {
                None => return None,
                Some(ref k) if k == key => return Some(&self.gens[i]),
                Some(_) => {}
            }
            i = (i + 1) & mask;
        }
    }

    /// Remove the entry at the probed slot `i`, shifting any following entries
    /// whose probe chain runs through the vacated slot one step back. This
    /// needs no tombstones: the table only ever holds `Some` (occupied) and
    /// `None` (empty), so probes always terminate and the load factor counts
    /// only occupied slots.
    fn delete_at(&mut self, i: usize, mask: usize)
    where
        K: Hash,
    {
        let mut i = i;
        // Walk forward from the gap. An entry at `j` must shift back into the
        // gap iff its probe chain (from its hash slot `h`) passes through the
        // gap `i`; otherwise it stays and the walk continues, because entries
        // further along may still chain through the gap.
        let mut j = (i + 1) & mask;
        while let Some(k) = &self.keys[j] {
            let h = Self::index(k, mask);
            let passes_gap = if h <= j {
                h <= i && i <= j
            } else {
                // The chain wraps: it covers `h..=mask` and `0..=j`.
                i >= h || i <= j
            };
            if passes_gap {
                self.keys[i] = self.keys[j].take();
                self.gens[i] = self.gens[j];
                i = j;
            }
            j = (j + 1) & mask;
        }
        self.keys[i] = None;
    }

    fn remove(&mut self, key: &K) -> Option<u64>
    where
        K: Eq + Hash,
    {
        if self.capacity() == 0 {
            return None;
        }
        let mask = self.capacity() - 1;
        let mut i = Self::index(key, mask);
        loop {
            match &self.keys[i] {
                None => return None,
                Some(k) if k == key => {
                    let generation = self.gens[i];
                    self.len -= 1;
                    self.delete_at(i, mask);
                    return Some(generation);
                }
                Some(_) => {}
            }
            i = (i + 1) & mask;
        }
    }

    /// Remove `key` iff it maps to exactly `generation`, in a single probe
    /// (the cancel path would otherwise pay get-then-remove, two probes).
    fn cancel(&mut self, key: &K, generation: u64) -> bool
    where
        K: Eq + Hash,
    {
        if self.capacity() == 0 {
            return false;
        }
        let mask = self.capacity() - 1;
        let mut i = Self::index(key, mask);
        loop {
            match &self.keys[i] {
                None => return false,
                Some(k) if k == key => {
                    if self.gens[i] == generation {
                        self.len -= 1;
                        self.delete_at(i, mask);
                        return true;
                    }
                    return false;
                }
                Some(_) => {}
            }
            i = (i + 1) & mask;
        }
    }

    fn shrink_to_fit(&mut self) {
        // The queue only shrinks after a full drain (len == 0), releasing the
        // whole table.
        if self.len == 0 {
            self.keys = Vec::new();
            self.gens = Vec::new();
            self.grow_threshold = 0;
        }
    }
}

/// Safe reference scheduler using a binary heap and generation index.
pub struct TimerQueue<I, K, V> {
    brand: Arc<QueueBrand>,
    heap: BinaryHeap<Reverse<Entry<I, K, V>>>,
    current: GenMap<K>,
    next_generation: u64,
    next_sequence: u64,
    /// Whether the heap may contain stale (superseded) entries. Stale entries
    /// are created only by replacing or cancelling a live schedule and are
    /// removed entirely by a compaction rebuild, so the flag tracks exactly
    /// those transitions. When `false`, `discard_stale` skips its per-pop
    /// generation lookup entirely.
    stale_possible: bool,
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
            brand: Arc::new(QueueBrand),
            heap: BinaryHeap::new(),
            current: GenMap::default(),
            next_generation: 1,
            next_sequence: 0,
            stale_possible: false,
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
    /// # Errors
    ///
    /// Returns [`ScheduleError::GenerationExhausted`] or
    /// [`ScheduleError::SequenceExhausted`] with the complete input when the
    /// corresponding counter has no next value. Rejection leaves the queue
    /// unchanged.
    ///
    #[inline]
    pub fn schedule(
        &mut self,
        key: K,
        at: I,
        value: V,
    ) -> Result<Token<K>, ScheduleError<I, K, V>> {
        let Some(next_generation) = self.next_generation.checked_add(1) else {
            return Err(ScheduleError::GenerationExhausted { key, at, value });
        };
        let Some(next_sequence) = self.next_sequence.checked_add(1) else {
            return Err(ScheduleError::SequenceExhausted { key, at, value });
        };

        let generation = self.next_generation;
        let sequence = self.next_sequence;
        self.next_generation = next_generation;
        self.next_sequence = next_sequence;
        if self.current.insert(key.clone(), generation).is_some() {
            // A replacement: the superseded generation stays in the heap.
            self.stale_possible = true;
        }
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
        Ok(Token {
            brand: Arc::clone(&self.brand),
            key,
            generation,
        })
    }

    /// Cancel exactly the generation named by `token`.
    ///
    /// Tokens are branded by their issuing queue: a token from another queue
    /// never cancels here, regardless of key and generation.
    #[inline]
    pub fn cancel(&mut self, token: &Token<K>) -> bool {
        if !Arc::ptr_eq(&self.brand, &token.brand) {
            return false;
        }
        if self.current.cancel(&token.key, token.generation) {
            // The cancelled entry remains in the heap until it surfaces.
            self.stale_possible = true;
            true
        } else {
            false
        }
    }

    /// Return the earliest current deadline, removing stale heap entries.
    #[inline]
    pub fn next_deadline(&mut self) -> Option<I> {
        self.discard_stale();
        self.heap.peek().map(|entry| entry.0.at)
    }

    /// Pop one current timer due at or before `now`.
    #[inline]
    pub fn pop_due(&mut self, now: I) -> Option<Expired<I, K, V>> {
        self.discard_stale();
        if self.heap.peek().is_none_or(|entry| entry.0.at > now) {
            return None;
        }
        let Reverse(entry) = self.heap.pop()?;
        self.current.remove(&entry.key);
        // Drain-release: once every key is popped the buffers still pin the
        // peak capacity. `compact()` covers the stale path; this covers the
        // clean path where `discard_stale` returns before calling it.
        if self.current.is_empty() {
            self.current.shrink_to_fit();
            self.heap.shrink_to_fit();
        }
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
    #[inline]
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
            // The rebuild discarded every stale entry.
            self.stale_possible = false;
        }
        if self.current.is_empty() {
            self.current.shrink_to_fit();
            self.heap.shrink_to_fit();
        }
    }

    #[inline]
    fn discard_stale(&mut self) {
        // `stale_possible` is false only when no stale entry can exist, so the
        // clean path skips both the compaction arithmetic and the per-pop
        // generation lookup. A live top does not imply the heap is clean
        // (stale entries lurk deeper), so the flag clears only when the heap
        // empties or a rebuild removes them.
        if !self.stale_possible {
            return;
        }
        self.compact();
        while self
            .heap
            .peek()
            .is_some_and(|entry| self.current.get(&entry.0.key) != Some(&entry.0.generation))
        {
            self.heap.pop();
        }
        if self.heap.is_empty() {
            self.stale_possible = false;
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
        // Multiple seeds broaden coverage of hash-layout-dependent bugs (the
        // tombstone load-factor hang was seed-dependent).
        for seed in [
            0xDEAD_BEEF_1234_u64,
            0x00C0_FFEE_0001,
            0x5EED_5EED_5EED,
            0x0102_0304_0506,
        ] {
            differential_run(seed);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the differential test enumerates every op type and phase explicitly"
    )]
    fn differential_run(seed: u64) {
        const KEYS: usize = 64;
        // Tombstone-sweep parameters (used after the main drain).
        const STRESS_KEYS: usize = 1024;
        const STRESS_BASE: u64 = 1_000_000;
        let mut rng = Rng(seed);
        let mut queue = TimerQueue::new();
        let mut tokens: Vec<Option<Token<u64>>> = vec![None; KEYS];
        // Reference model: per-key live schedule and a set ordered by deadline.
        let mut live: BTreeMap<u64, (u64, u64, u64)> = BTreeMap::new(); // key -> (at, gen, value)
        let mut by_deadline: BTreeSet<(u64, u64, u64)> = BTreeSet::new(); // (at, gen, key)
        let mut modelpopped_gen = 0_u64;

        // Keep the same seeded state machine under Miri, but bound the
        // interpreter run. Native CI retains the full eight-million-operation
        // differential plus tombstone sweep.
        let operations = if cfg!(miri) { 5_000 } else { 50_000 };
        for _ in 0..operations {
            let key = (rng.next() % KEYS as u64) as usize;
            let roll = rng.next() % 10;
            match roll {
                0..=5 => {
                    let at = rng.next() % 100;
                    let value = rng.next();
                    let token = queue.schedule(key as u64, at, value).unwrap();
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
            let (at, _seq_gen, k) = by_deadline.pop_first().expect("model exhausted early");
            assert_eq!(expired.at, at);
            assert_eq!(expired.key, k);
        }
        assert!(by_deadline.is_empty(), "model should be empty after drain");
        assert!(queue.is_empty());

        // Tombstone stress: schedule-then-cancel sweeps over a wide key
        // range. The map's occupied count stays near zero while tombstones
        // from many distinct keys accumulate; a growth condition that
        // ignores tombstones lets the probe table fill and the probes spin
        // forever (caught as a 600x mixed-workload slowdown before the
        // fix). The queue must stay empty throughout the sweep.
        let stress_rounds = if cfg!(miri) { 20 } else { 2_000 };
        let stress_keys = if cfg!(miri) { 64 } else { STRESS_KEYS };
        for round in 0..stress_rounds {
            for key in 0..stress_keys {
                let s_key = STRESS_BASE + u64::try_from(key).expect("key fits u64");
                let token = queue.schedule(s_key, round % 100, round).unwrap();
                assert!(queue.cancel(&token), "sweep cancel must succeed");
            }
            assert_eq!(
                queue.len(),
                0,
                "sweep must leave nothing live at round {round}"
            );
        }
        assert!(
            queue.next_deadline().is_none(),
            "all sweep entries are stale; deadline must be none"
        );
    }

    /// `&str` keys exercise the non-integer hash path of the generation map.
    #[test]
    fn str_keys_schedule_replace_cancel_pop() {
        let mut queue = TimerQueue::new();
        let old = queue.schedule("actor", 10_u64, "old").unwrap();
        let new = queue.schedule("actor", 20, "new").unwrap();
        assert!(!queue.cancel(&old));
        assert_eq!(queue.next_deadline(), Some(20));
        assert_eq!(queue.pop_due(19), None);
        assert_eq!(queue.pop_due(20).unwrap().value, "new");
        assert!(!queue.cancel(&new));
    }

    #[test]
    fn generation_exhaustion_returns_input_and_preserves_queue_state() {
        let mut queue = TimerQueue::new();
        let existing = queue
            .schedule(String::from("existing"), 11_u64, String::from("kept"))
            .unwrap();
        queue.next_generation = u64::MAX;
        queue.next_sequence = 17;

        let error = queue
            .schedule(String::from("actor"), 23_u64, String::from("payload"))
            .unwrap_err();

        assert_eq!(
            error,
            ScheduleError::GenerationExhausted {
                key: String::from("actor"),
                at: 23,
                value: String::from("payload"),
            }
        );
        assert_eq!(queue.next_generation, u64::MAX);
        assert_eq!(queue.next_sequence, 17);
        assert_eq!(queue.current.len(), 1);
        assert_eq!(
            queue.current.get(existing.key()),
            Some(&existing.generation)
        );
        assert_eq!(queue.heap.len(), 1);
        assert!(!queue.stale_possible);
        assert_eq!(
            queue.pop_due(11),
            Some(Expired {
                at: 11,
                key: String::from("existing"),
                value: String::from("kept"),
            })
        );
    }

    #[test]
    fn sequence_exhaustion_returns_input_without_advancing_generation() {
        let mut queue = TimerQueue::new();
        queue.next_generation = 41;
        queue.next_sequence = u64::MAX;

        let error = queue.schedule("actor", 23_u64, "payload").unwrap_err();

        assert_eq!(
            error,
            ScheduleError::SequenceExhausted {
                key: "actor",
                at: 23,
                value: "payload",
            }
        );
        assert_eq!(queue.next_generation, 41);
        assert_eq!(queue.next_sequence, u64::MAX);
        assert!(queue.current.is_empty());
        assert!(queue.heap.is_empty());
        assert!(!queue.stale_possible);
    }

    #[test]
    fn failed_replacement_leaves_existing_schedule_current() {
        let mut queue = TimerQueue::new();
        let token = queue.schedule("actor", 10_u64, "existing").unwrap();
        queue.next_sequence = u64::MAX;

        assert_eq!(
            queue.schedule("actor", 20, "replacement"),
            Err(ScheduleError::SequenceExhausted {
                key: "actor",
                at: 20,
                value: "replacement",
            })
        );
        assert_eq!(queue.current.get(token.key()), Some(&token.generation));
        assert_eq!(queue.next_deadline(), Some(10));
        assert_eq!(
            queue.pop_due(10),
            Some(Expired {
                at: 10,
                key: "actor",
                value: "existing",
            })
        );
    }

    #[test]
    fn successful_schedule_returns_exact_queue_branded_token() {
        let mut queue = TimerQueue::new();

        let token = queue.schedule("actor", 10_u64, "value").unwrap();

        assert!(Arc::ptr_eq(&token.brand, &queue.brand));
        assert_eq!(token.key, "actor");
        assert_eq!(token.generation, 1);
        assert!(queue.cancel(&token));
    }
}
