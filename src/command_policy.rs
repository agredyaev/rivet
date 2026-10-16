//! Pure command-admission predicates shared by the runtime and fuzz targets.

pub fn arguments_valid(args: &[String]) -> bool {
    !args.iter().any(|arg| arg.contains('\0'))
}

pub fn timeout_valid(timeout_ms: u64, max_timeout_ms: u64) -> bool {
    timeout_ms != 0 && timeout_ms <= max_timeout_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_and_timeout_bounds_are_strict() {
        assert!(arguments_valid(&["a".into(), "b".into()]));
        assert!(!arguments_valid(&["a\0b".into()]));
        assert!(!timeout_valid(0, 1_000));
        assert!(timeout_valid(1, 1_000));
        assert!(timeout_valid(1_000, 1_000));
        assert!(!timeout_valid(1_001, 1_000));
    }
}
