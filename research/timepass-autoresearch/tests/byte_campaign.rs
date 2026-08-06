//! Deterministic byte-stream campaign: the same byte→op grammar the fuzz
//! target exposes to libFuzzer, driven here by seeded pseudo-random streams
//! so the campaign is reproducible without the fuzzer. Set `ADV_STREAMS` and
//! `ADV_MAX_LEN` to scale a run; the defaults keep the check gate fast.

use timepass_autoresearch::byte_campaign;

fn streams() -> u64 {
    std::env::var("ADV_STREAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(miri) { 64 } else { 4_096 })
}

fn max_len() -> usize {
    std::env::var("ADV_MAX_LEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_024)
}

#[test]
fn byte_campaign_seed_1() {
    let done = byte_campaign(streams(), max_len(), 0x5EED_0001);
    assert_eq!(done, streams());
}

#[test]
fn byte_campaign_seed_2() {
    let done = byte_campaign(streams(), max_len(), 0x5EED_0002);
    assert_eq!(done, streams());
}
