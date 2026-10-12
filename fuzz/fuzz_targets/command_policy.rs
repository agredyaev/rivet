#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use rivet::command_policy::{arguments_valid, subcommand_allowed, timeout_valid};

fn next<T: for<'a> Arbitrary<'a> + Default>(input: &mut Unstructured<'_>) -> T {
    T::arbitrary(input).unwrap_or_default()
}

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let allowed: Vec<String> = next(&mut input);
    let args: Vec<String> = next(&mut input);
    let restricted = next::<bool>(&mut input);

    let policy = restricted.then_some(allowed.as_slice());
    let admitted = subcommand_allowed(policy, &args);

    if !restricted {
        assert!(admitted);
    } else if admitted {
        let first = args.first().expect("restricted admission requires argv[0]");
        assert!(allowed.iter().any(|item| item == first));
    } else if let Some(first) = args.first() {
        assert!(!allowed.iter().any(|item| item == first) || allowed.is_empty());
    }

    assert_eq!(
        arguments_valid(&args),
        !args.iter().any(|arg| arg.as_bytes().contains(&0))
    );

    let timeout = next::<u64>(&mut input);
    let max = next::<u64>(&mut input);
    assert_eq!(timeout_valid(timeout, max), timeout != 0 && timeout <= max);
});
