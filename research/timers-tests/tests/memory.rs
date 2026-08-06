//! Memory retention and reclamation: the queue must release its buffers after
//! a full drain and after cancelled entries are discarded. A counting global
//! allocator measures retained bytes; thresholds sit far below the workload
//! peak so only genuine retention (pinning the peak) fails.
//!
//! Allocator invariant: `System` is the backing allocator; the counter tracks
//! exact `Layout::size` on every alloc/dealloc pair, so a zero delta means
//! every byte was returned. `realloc` uses the trait default (alloc + copy +
//! dealloc), which is accounted through the same two hooks.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use bombay_timers::TimerQueue;

struct Counting;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to `System` with matching layout; the counter
// is atomic and only ever observes the real (de)allocation sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The counter is process-global, so only one measurement may run at a time
/// within this test binary (the default multi-threaded harness would
/// otherwise attribute a sibling test's live allocations to this one).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn live() -> i64 {
    // Signed: sibling harness structures may free inside a measurement
    // window, so a delta can legitimately be negative (net release).
    LIVE_BYTES.load(Ordering::Relaxed) as i64
}

fn scale(native: u64) -> u64 {
    if cfg!(miri) { native / 16 } else { native }
}

/// After a full drain the queue must not pin the workload's peak footprint.
#[test]
fn memory_released_after_full_drain() {
    let _serial = SERIAL.lock().expect("serial lock poisoned");
    let n = scale(50_000);
    let base = live();
    {
        let mut queue = TimerQueue::new();
        for key in 0..n {
            queue.schedule(key, key % 1_000, Box::new([0_u8; 64]));
        }
        let peak = live() - base;
        while queue.pop_due(u64::MAX).is_some() {}
        let retained = live() - base;
        assert!(
            retained < peak / 4,
            "retained {retained} of peak {peak} bytes after full drain"
        );
    }
    let retained_after_drop = live() - base;
    assert!(
        retained_after_drop < 64 * 1024,
        "queue drop retained {retained_after_drop} bytes"
    );
}

/// Cancelled values are documented to stay in the heap until they surface;
/// once discarded (here via `next_deadline`) their memory must be released.
#[test]
fn cancelled_values_held_until_surfaced_then_released() {
    let _serial = SERIAL.lock().expect("serial lock poisoned");
    let n = scale(4_096);
    let base = live();
    let mut queue = TimerQueue::new();
    let mut tokens = Vec::new();
    for key in 0..n {
        tokens.push(queue.schedule(key, 1_000_000_u64, Box::new([7_u8; 256])));
    }
    for token in &tokens {
        assert!(queue.cancel(token));
    }
    let held = live() - base;
    assert!(
        held > n as i64 * 200,
        "cancelled values should still be held (documented laziness), held {held}"
    );
    assert_eq!(queue.next_deadline(), None);
    let after = live() - base;
    assert!(
        after < n as i64 * 32,
        "retained {after} bytes after stale discard (held {held} before)"
    );
    drop(tokens);
}

/// Each queue carries exactly one small brand allocation.
#[test]
fn queue_allocation_cost_is_one_small_brand() {
    let _serial = SERIAL.lock().expect("serial lock poisoned");
    let n: i64 = scale(1_000) as i64;
    let base = live();
    let queues: Vec<TimerQueue<u64, u64, u64>> = (0..n).map(|_| TimerQueue::new()).collect();
    let held = live() - base;
    assert_eq!(queues.len() as i64, n);
    // Subtract the container: the Vec holds the queue structs by value.
    let container = n * size_of::<TimerQueue<u64, u64, u64>>() as i64;
    let marginal = held - container;
    // One ArcInner (two usize refcounts, ZST payload) per queue: 16 bytes.
    // Allow generous slack for allocator rounding, but reject any design that
    // allocates more than a small fixed block per queue.
    assert!(
        marginal <= n * 64,
        "queue brand allocation too large: {marginal} marginal bytes for {n} queues"
    );
    drop(queues);
    let after = live() - base;
    assert!(after <= n * 8, "brands not released with queues: {after}");
}

/// Replace-heavy churn must compact: retained bytes after the final drain
/// stay near zero even though the heap transiently held many stale entries.
#[test]
fn replace_churn_compacts_and_releases() {
    let _serial = SERIAL.lock().expect("serial lock poisoned");
    let rounds = scale(20_000);
    let base = live();
    let mut queue = TimerQueue::new();
    for round in 0..rounds {
        queue.schedule("only-key", round % 512, Box::new([1_u8; 128]));
    }
    let fired = queue.pop_due(511).expect("due");
    assert_eq!(fired.at, (rounds - 1) % 512);
    drop(fired);
    let retained = live() - base;
    assert!(
        retained < 64 * 1024,
        "replace churn retained {retained} bytes after final pop"
    );
    assert!(queue.is_empty());
}
