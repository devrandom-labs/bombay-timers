use criterion::{Criterion, criterion_group, criterion_main};
use timepass::TimerQueue;

fn schedule_and_expire(c: &mut Criterion) {
    c.bench_function("schedule_expire_64k", |b| {
        b.iter(|| {
            let mut queue = TimerQueue::new();
            for key in 0..65_536_u64 {
                queue.schedule(key, key, key);
            }
            while let Some(expired) = queue.pop_due(u64::MAX) {
                std::hint::black_box(expired);
            }
        });
    });
}
criterion_group!(benches, schedule_and_expire);
criterion_main!(benches);
