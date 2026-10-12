#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use rivet::path_policy::relative_under;
use std::path::{Component, Path, PathBuf};

const ALNUM: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn next<T: for<'a> Arbitrary<'a> + Default>(input: &mut Unstructured<'_>) -> T {
    T::arbitrary(input).unwrap_or_default()
}

fn component(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().clamp(1, 32));
    if bytes.is_empty() {
        out.push('x');
        return out;
    }
    for byte in bytes.iter().take(32) {
        out.push(ALNUM[(*byte as usize) % ALNUM.len()] as char);
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let root_part = component(&next::<Vec<u8>>(&mut input));
    let child_a = component(&next::<Vec<u8>>(&mut input));
    let child_b = component(&next::<Vec<u8>>(&mut input));

    let root = PathBuf::from("/workspace").join(&root_part);
    let relative = PathBuf::from(&child_a).join(&child_b);
    let inside = root.join(&relative);

    let admitted = relative_under(&root, &inside).expect("child path must be admitted");
    assert_eq!(admitted, relative.as_path());
    assert!(admitted.components().all(|part| matches!(part, Component::Normal(_))));

    assert_eq!(relative_under(&root, &root), Some(Path::new("")));

    let escape = root.join("..").join(&child_a);
    assert_eq!(relative_under(&root, &escape), None);

    let sibling = PathBuf::from("/workspace").join(format!("{root_part}-other")).join(&child_a);
    assert_eq!(relative_under(&root, &sibling), None);

    let relative_input = PathBuf::from(&root_part).join(&child_a);
    assert_eq!(relative_under(&root, &relative_input), None);
});
