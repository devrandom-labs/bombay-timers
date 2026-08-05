//! A keyed, generation-safe monotonic timer queue.

use core::cmp::{Ordering, Reverse};
use core::hash::Hash;
use std::collections::{BinaryHeap, HashMap};

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
    current: HashMap<K, u64>,
    next_generation: u64,
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
            current: HashMap::new(),
            next_generation: 1,
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
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
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

    fn discard_stale(&mut self) {
        while self
            .heap
            .peek()
            .is_some_and(|entry| self.current.get(&entry.0.key) != Some(&entry.0.generation))
        {
            self.heap.pop();
        }
    }
}
