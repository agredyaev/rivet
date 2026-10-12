#![no_main]

use libfuzzer_sys::fuzz_target;
use rivet::tunnel_policy::{PostClass, backoff_base_ms, classify_post};

fn expected(status: u16) -> PostClass {
    match status {
        200 => PostClass::Accepted,
        404 => PostClass::Terminal,
        401 | 403 => PostClass::Auth,
        408 | 429 | 502 | 503 | 504 => PostClass::Retry,
        _ => PostClass::Fail,
    }
}

fuzz_target!(|data: &[u8]| {
    let mut attempt = 0_u32;
    let mut previous_base = 0_u64;
    let mut terminal_seen = false;

    for chunk in data.chunks(2).take(512) {
        let status = u16::from_le_bytes([chunk[0], *chunk.get(1).unwrap_or(&0)]);
        let class = classify_post(status);
        assert_eq!(class, expected(status));

        if terminal_seen {
            continue;
        }
        if class == PostClass::Retry {
            attempt = attempt.saturating_add(1);
            let base = backoff_base_ms(attempt);
            assert!((200..=10_000).contains(&base));
            assert!(base >= previous_base);
            previous_base = base;
        } else {
            terminal_seen = true;
        }
    }

    assert_eq!(backoff_base_ms(0), 200);
    assert_eq!(backoff_base_ms(1), 200);
    assert_eq!(backoff_base_ms(u32::MAX), 10_000);
});
