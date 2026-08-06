//! Deterministic workload harness for measuring bombay-timers.
//!
//! Each subcommand runs one fixed, seeded workload and prints `METRIC`
//! lines. Run with the same `RUSTFLAGS=-C target-cpu=native` as
//! `the benchmark harness` for comparable numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Instant;

use bombay_timers::{TimerQueue, Token};

/// Deterministic xorshift64* generator: fixed seeds, no external state.
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

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static IN_USE: AtomicUsize = AtomicUsize::new(0);

/// Global allocator that counts allocations and net in-use bytes.
///
/// # Safety
/// Every method forwards to the system allocator with the exact layout it
/// received, so the delegation is sound. The counters use relaxed ordering:
/// they are measurement state, never synchronization.
struct Counting;

// SAFETY: pure delegation to `System`; layouts are passed through unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        IN_USE.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded to the system allocator with the same layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        IN_USE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded with the layout from a matching allocation.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        if new_size > layout.size() {
            IN_USE.fetch_add(new_size - layout.size(), Ordering::Relaxed);
        } else {
            IN_USE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        // SAFETY: forwarded with the original layout and the new size.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        IN_USE.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded to the system allocator with the same layout.
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn reset_counters() {
    ALLOCS.store(0, Ordering::Relaxed);
    IN_USE.store(0, Ordering::Relaxed);
}

fn counters() -> (usize, usize) {
    (
        ALLOCS.load(Ordering::Relaxed),
        IN_USE.load(Ordering::Relaxed),
    )
}

fn in_use() -> usize {
    IN_USE.load(Ordering::Relaxed)
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    assert!(!sorted.is_empty(), "percentile of an empty sample set");
    // The sample sizes here never exceed 2^53, so the float index is exact
    // enough for p50/p99 reporting; truncation is the intended rounding.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sample sizes are far below 2^53; truncation rounds the index"
    )]
    let idx = ((sorted.len() - 1) as f64 * p) as usize;
    sorted[idx]
}

fn report_percentiles(label: &str, samples: &mut [u64]) {
    samples.sort_unstable();
    println!("METRIC {label}_p50_ns={}", percentile(samples, 0.50));
    println!("METRIC {label}_p99_ns={}", percentile(samples, 0.99));
    println!("METRIC {label}_p999_ns={}", percentile(samples, 0.999));
}

fn main() {
    let workload = std::env::args().nth(1).unwrap_or_else(|| "schedule".into());
    match workload.as_str() {
        "schedule" => workload_schedule(1_000_000),
        "replace" => workload_replace(1_000_000, 8),
        "cancel" => workload_cancel(1_000_000),
        "mixed" => workload_mixed(200_000, 2_000_000),
        "latency" => workload_latency(1_000_000),
        "scale" => workload_scale(),
        "concurrent" => workload_concurrent(8, 200_000),
        other => panic!("unknown workload: {other}"),
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_schedule(keys: u64) {
    let mut queue = TimerQueue::new();
    reset_counters();
    let started = Instant::now();
    for key in 0..keys {
        queue.schedule(key, key % 65_536, key);
    }
    let peak = in_use();
    let mut fires = 0_u64;
    while let Some(expired) = queue.pop_due(u64::MAX) {
        fires += 1;
        black_box(expired);
    }
    let elapsed = started.elapsed();
    let (allocs, retained) = counters();
    assert_eq!(fires, keys, "every scheduled timer fires exactly once");
    println!(
        "METRIC schedule_timers_per_second={:.1}",
        keys as f64 / elapsed.as_secs_f64()
    );
    println!(
        "METRIC schedule_allocs_per_timer={:.6}",
        allocs as f64 / keys as f64
    );
    println!(
        "METRIC schedule_peak_bytes_per_timer={:.3}",
        peak as f64 / keys as f64
    );
    println!("METRIC schedule_retained_bytes_after_drain={retained}");
    black_box(&mut queue);
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_replace(keys: u64, replaces: u64) {
    let mut rng = Rng(0xD1CE_5EED_1234);
    let mut queue = TimerQueue::new();
    reset_counters();
    let started = Instant::now();
    for key in 0..keys {
        queue.schedule(key, rng.next() % 1_000_000, key);
    }
    for _ in 0..replaces {
        for key in 0..keys {
            queue.schedule(key, rng.next() % 1_000_000, key);
        }
    }
    let peak = in_use();
    let mut fires = 0_u64;
    while let Some(expired) = queue.pop_due(u64::MAX) {
        fires += 1;
        black_box(expired);
    }
    let elapsed = started.elapsed();
    let (allocs, retained) = counters();
    assert_eq!(fires, keys, "exactly the live generation of each key fires");
    let schedules = keys * (replaces + 1);
    let ops = schedules + fires;
    println!(
        "METRIC replace_ops_per_second={:.1}",
        ops as f64 / elapsed.as_secs_f64()
    );
    println!(
        "METRIC replace_allocs_per_schedule={:.6}",
        allocs as f64 / schedules as f64
    );
    println!(
        "METRIC replace_peak_bytes_per_live={:.3}",
        peak as f64 / keys as f64
    );
    println!("METRIC replace_retained_bytes_after_drain={retained}");
    black_box(&mut queue);
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_cancel(keys: u64) {
    let mut tokens: Vec<Token<u64>> = Vec::with_capacity(keys as usize);
    let mut queue = TimerQueue::new();
    reset_counters();
    for key in 0..keys {
        tokens.push(queue.schedule(key, key % 65_536, key));
    }
    let started = Instant::now();
    let mut cancels = 0_u64;
    for token in &tokens {
        if queue.cancel(token) {
            cancels += 1;
        }
    }
    let elapsed = started.elapsed();
    let stale_drain = Instant::now();
    assert!(queue.pop_due(u64::MAX).is_none(), "all entries are stale");
    let stale_drain_ns = stale_drain.elapsed().as_nanos();
    let (allocs, retained) = counters();
    assert_eq!(cancels, keys, "every live token cancels");
    println!(
        "METRIC cancel_ops_per_second={:.1}",
        keys as f64 / elapsed.as_secs_f64()
    );
    println!(
        "METRIC cancel_allocs_per_key={:.6}",
        allocs as f64 / keys as f64
    );
    println!("METRIC cancel_retained_bytes_after_cancel={retained}");
    println!("METRIC cancel_stale_drain_ns={stale_drain_ns}");
    black_box(&mut queue);
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_latency(keys: u64) {
    let mut queue = TimerQueue::new();
    for key in 0..keys {
        queue.schedule(key, key, key);
    }
    let mut samples: Vec<u64> = Vec::with_capacity(keys as usize);
    for i in 0..keys {
        let started = Instant::now();
        queue.schedule(i, i, i);
        samples.push(started.elapsed().as_nanos() as u64);
    }
    report_percentiles("replace", &mut samples);

    let mut tokens: Vec<Token<u64>> = Vec::with_capacity(keys as usize);
    for key in 0..keys {
        tokens.push(queue.schedule(key, key, key));
    }
    let mut samples: Vec<u64> = Vec::with_capacity(keys as usize);
    for token in &tokens {
        let started = Instant::now();
        queue.cancel(token);
        samples.push(started.elapsed().as_nanos() as u64);
    }
    report_percentiles("cancel", &mut samples);

    for key in 0..keys {
        queue.schedule(key, key, key);
    }
    let mut samples: Vec<u64> = Vec::with_capacity(keys as usize);
    for _ in 0..keys {
        let started = Instant::now();
        let expired = queue.pop_due(u64::MAX);
        samples.push(started.elapsed().as_nanos() as u64);
        black_box(expired);
    }
    report_percentiles("pop_due", &mut samples);
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_mixed(actors: u64, steps: u64) {
    let mut rng = Rng(0x5EED_BEAF_1234);
    let mut queue = TimerQueue::new();
    let mut state: Vec<Option<Token<u64>>> = vec![None; actors as usize];
    let mut now = 0_u64;
    let mut schedules = 0_u64;
    let mut replaces = 0_u64;
    let mut cancels = 0_u64;
    let mut fires = 0_u64;
    let mut peak_step_fires = 0_u64;
    reset_counters();
    let started = Instant::now();
    for step in 0..steps {
        now = now.wrapping_add(rng.next() % 3);
        let actor = (rng.next() % actors) as usize;
        let roll = rng.next() % 100;
        let occupied = state[actor].is_some();
        if roll < 55 {
            let deadline = now.wrapping_add(1 + rng.next() % 1_000);
            let token = queue.schedule(actor as u64, deadline, actor as u64);
            state[actor] = Some(token);
            if occupied {
                replaces += 1;
            } else {
                schedules += 1;
            }
        } else if occupied
            && roll < 75
            && let Some(token) = state[actor].take()
            && queue.cancel(&token)
        {
            cancels += 1;
        }
        let mut step_fires = 0_u64;
        while let Some(expired) = queue.pop_due(now) {
            state[expired.key as usize] = None;
            fires += 1;
            step_fires += 1;
            black_box(expired);
        }
        peak_step_fires = peak_step_fires.max(step_fires);
        if step % 100_000 == 99_999 {
            for _ in 0..1_000 {
                let actor = (rng.next() % actors) as usize;
                let token = queue.schedule(actor as u64, now.wrapping_add(50), actor as u64);
                if state[actor].replace(token).is_some() {
                    replaces += 1;
                } else {
                    schedules += 1;
                }
            }
        }
    }
    let mut final_fires = 0_u64;
    while let Some(expired) = queue.pop_due(u64::MAX) {
        final_fires += 1;
        black_box(expired);
    }
    let elapsed = started.elapsed();
    let (allocs, retained) = counters();
    assert_eq!(
        schedules,
        fires + cancels + final_fires,
        "every schedule generation fires or is cancelled exactly once"
    );
    fires += final_fires;
    let ops = schedules + replaces + cancels + fires;
    println!(
        "METRIC mixed_ops_per_second={:.1}",
        ops as f64 / elapsed.as_secs_f64()
    );
    println!("METRIC mixed_fires={fires}");
    println!("METRIC mixed_peak_step_fires={peak_step_fires}");
    println!(
        "METRIC mixed_allocs_per_op={:.6}",
        allocs as f64 / ops as f64
    );
    println!("METRIC mixed_retained_bytes_after_drain={retained}");
    black_box(&mut queue);
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_scale() {
    for keys in [1_000_u64, 10_000, 100_000, 1_000_000] {
        let mut queue = TimerQueue::new();
        reset_counters();
        let started = Instant::now();
        for key in 0..keys {
            queue.schedule(key, key % 65_536, key);
        }
        let peak = in_use();
        while let Some(expired) = queue.pop_due(u64::MAX) {
            black_box(expired);
        }
        let elapsed = started.elapsed();
        println!(
            "METRIC scale_{keys}_timers_per_second={:.1}",
            keys as f64 / elapsed.as_secs_f64()
        );
        println!(
            "METRIC scale_{keys}_peak_bytes_per_timer={:.3}",
            peak as f64 / keys as f64
        );
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "metric arithmetic over u64 counters"
)]
fn workload_concurrent(threads: usize, ops: u64) {
    let queue = Arc::new(Mutex::new(TimerQueue::new()));
    let barrier = Arc::new(Barrier::new(threads));
    reset_counters();
    let started = Instant::now();
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let queue = Arc::clone(&queue);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let tid = u64::try_from(t).expect("thread id fits u64");
                let mut rng = Rng(0xC0FF_EE00_0000 + tid);
                barrier.wait();
                let mut pending: Vec<Token<u64>> = Vec::with_capacity(64);
                let mut schedules = 0_u64;
                let mut cancels = 0_u64;
                let mut fires = 0_u64;
                for i in 0..ops {
                    let key = (tid << 32) | i;
                    match rng.next() % 10 {
                        0..=4 => {
                            let token = queue
                                .lock()
                                .expect("queue mutex poisoned")
                                .schedule(key, i, key);
                            schedules += 1;
                            if pending.len() >= 64 {
                                pending.swap_remove(0);
                            }
                            pending.push(token);
                        }
                        5..=7 => {
                            if let Some(token) = pending.pop()
                                && queue.lock().expect("queue mutex poisoned").cancel(&token)
                            {
                                cancels += 1;
                            }
                        }
                        _ => {
                            let mut guard = queue.lock().expect("queue mutex poisoned");
                            while let Some(expired) = guard.pop_due(i) {
                                fires += 1;
                                black_box(expired);
                            }
                        }
                    }
                }
                (schedules, cancels, fires)
            })
        })
        .collect();
    let mut schedules = 0_u64;
    let mut cancels = 0_u64;
    let mut fires = 0_u64;
    for handle in handles {
        let (s, c, f) = handle.join().expect("worker panicked");
        schedules += s;
        cancels += c;
        fires += f;
    }
    let elapsed = started.elapsed();
    let (allocs, _retained) = counters();
    let ops_done = u64::try_from(threads).expect("thread count fits u64") * ops;
    println!(
        "METRIC concurrent_ops_per_second={:.1}",
        ops_done as f64 / elapsed.as_secs_f64()
    );
    println!("METRIC concurrent_schedules={schedules}");
    println!("METRIC concurrent_cancels={cancels}");
    println!("METRIC concurrent_fires={fires}");
    println!(
        "METRIC concurrent_allocs_per_op={:.6}",
        allocs as f64 / ops_done as f64
    );
}
