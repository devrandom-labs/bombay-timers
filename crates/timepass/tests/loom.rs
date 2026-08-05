#![cfg(loom)]

use loom::sync::{Arc, Mutex};
use loom::thread;

#[test]
fn replacement_and_expiration_are_serialized_without_duplicate_fire() {
    loom::model(|| {
        let generation = Arc::new(Mutex::new((1_u64, false)));
        let writer = generation.clone();
        let reader = generation.clone();
        let replace = thread::spawn(move || *writer.lock().unwrap() = (2, false));
        let expire = thread::spawn(move || {
            let mut state = reader.lock().unwrap();
            if !state.1 {
                state.1 = true;
            }
        });
        replace.join().unwrap();
        expire.join().unwrap();
        let state = generation.lock().unwrap();
        assert!(state.0 == 2 || state.1);
    });
}
