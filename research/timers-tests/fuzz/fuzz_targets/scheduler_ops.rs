//! Coverage-guided fuzz target: arbitrary bytes drive the same op grammar as
//! the deterministic byte campaign, checked step-by-step against the
//! independent model. A crash input reproduces with
//! `timers_tests::run_bytes(&campaign_space(), &data[1..], narrow)`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use timers_tests::{campaign_space, run_bytes};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let narrow = data[0] & 1 == 1;
    run_bytes(&campaign_space(), &data[1..], narrow);
});
