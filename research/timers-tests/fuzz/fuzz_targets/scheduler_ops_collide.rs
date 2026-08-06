//! Coverage-guided fuzz target over the all-colliding key space: maximal
//! probe-chain clustering in the generation map under arbitrary op streams.
#![no_main]

use libfuzzer_sys::fuzz_target;
use timers_tests::{CollideKey, run_bytes};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let narrow = data[0] & 1 == 1;
    let space: Vec<CollideKey> = (0..8).map(CollideKey).collect();
    run_bytes(&space, &data[1..], narrow);
});
