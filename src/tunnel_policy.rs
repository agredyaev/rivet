//! Pure terminal-response policy shared by the tunnel runtime and fuzz targets.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostClass {
    Accepted,
    Terminal,
    Retry,
    Auth,
    Fail,
}

pub fn classify_post(status: u16) -> PostClass {
    match status {
        200 => PostClass::Accepted,
        404 => PostClass::Terminal,
        401 | 403 => PostClass::Auth,
        408 | 429 | 502 | 503 | 504 => PostClass::Retry,
        _ => PostClass::Fail,
    }
}

pub fn backoff_base_ms(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(6);
    200_u64.saturating_mul(1_u64 << shift).min(10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_classes_match_protocol() {
        assert_eq!(classify_post(200), PostClass::Accepted);
        assert_eq!(classify_post(404), PostClass::Terminal);
        for status in [408, 429, 502, 503, 504] {
            assert_eq!(classify_post(status), PostClass::Retry);
        }
        for status in [401, 403] {
            assert_eq!(classify_post(status), PostClass::Auth);
        }
        for status in [0, 201, 400, 500, u16::MAX] {
            assert_eq!(classify_post(status), PostClass::Fail);
        }
    }

    #[test]
    fn backoff_base_is_bounded_and_saturates() {
        assert_eq!(backoff_base_ms(0), 200);
        assert_eq!(backoff_base_ms(1), 200);
        assert_eq!(backoff_base_ms(2), 400);
        assert_eq!(backoff_base_ms(6), 6_400);
        assert_eq!(backoff_base_ms(7), 10_000);
        assert_eq!(backoff_base_ms(u32::MAX), 10_000);
    }
}
