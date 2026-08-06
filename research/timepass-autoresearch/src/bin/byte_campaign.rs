//! Long deterministic byte-stream campaign runner (release-mode runs are
//! reported in RESEARCH-REPORT.md). Usage:
//! `ADV_STREAMS=1000000 ADV_MAX_LEN=4096 cargo run --release --bin byte_campaign`

use timepass_autoresearch::byte_campaign;

fn main() {
    let streams: u64 = std::env::var("ADV_STREAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let max_len: usize = std::env::var("ADV_MAX_LEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_096);
    let seed: u64 = std::env::var("ADV_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5EED_5EED);
    let done = byte_campaign(streams, max_len, seed);
    println!("byte campaign complete: {done} streams, max_len {max_len}, seed {seed:#x}");
}
