#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use rivet::command_policy::{arguments_valid, timeout_valid};

fn next<T: for<'a> Arbitrary<'a> + Default>(input: &mut Unstructured<'_>) -> T {
    T::arbitrary(input).unwrap_or_default()
}

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let args: Vec<String> = next(&mut input);

    assert_eq!(
        arguments_valid(&args),
        !args.iter().any(|arg| arg.as_bytes().contains(&0))
    );

    let timeout = next::<u64>(&mut input);
    let max = next::<u64>(&mut input);
    assert_eq!(timeout_valid(timeout, max), timeout != 0 && timeout <= max);
});
