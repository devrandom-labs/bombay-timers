use std::time::Instant;

use timepass::TimerQueue;

const OPERATIONS: u32 = 1_000_000;

fn main() {
    let started = Instant::now();
    let mut queue = TimerQueue::new();
    for key in 0..OPERATIONS {
        queue.schedule(key, u64::from(key % 65_536), key);
    }
    while let Some(expired) = queue.pop_due(u64::MAX) {
        std::hint::black_box(expired);
    }
    let score = f64::from(OPERATIONS) / started.elapsed().as_secs_f64();
    println!("SCORE={score:.3}");
    println!("TIMERS_PER_SECOND={score:.3}");
}
