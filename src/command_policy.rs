//! Pure command-admission predicates shared by the runtime and fuzz targets.

pub fn subcommand_allowed(allowed: Option<&[String]>, args: &[String]) -> bool {
    allowed.is_none_or(|allowed| {
        args.first()
            .is_some_and(|first| allowed.iter().any(|item| item == first))
    })
}

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
    fn restricted_subcommand_requires_exact_first_argument() {
        let allowed = vec!["status".to_owned(), "diff".to_owned()];
        assert!(subcommand_allowed(Some(&allowed), &["status".into()]));
        assert!(!subcommand_allowed(Some(&allowed), &[]));
        assert!(!subcommand_allowed(Some(&allowed), &["Status".into()]));
        assert!(!subcommand_allowed(Some(&allowed), &["status\0x".into()]));
        assert!(subcommand_allowed(None, &[]));
    }

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
