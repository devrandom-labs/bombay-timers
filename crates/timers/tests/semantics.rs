use bombay_timers::TimerQueue;

#[test]
fn replacement_invalidates_old_generation_and_never_fires_early() {
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
fn equal_deadlines_fire_in_schedule_order() {
    let mut queue = TimerQueue::new();
    queue.schedule(1, 10_u64, "first").unwrap();
    queue.schedule(2, 10, "second").unwrap();
    assert_eq!(queue.pop_due(10).unwrap().value, "first");
    assert_eq!(queue.pop_due(10).unwrap().value, "second");
}
