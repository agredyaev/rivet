use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn production_prefix(text: &str) -> &str {
    let mut end = text.len();
    for marker in [
        "
#[cfg(test)]
mod ",
        "
#[cfg(all(test, unix))]
mod ",
        "
#[cfg(all(test, any(target_os = \"macos\", target_os = \"linux\")))]
mod ",
    ] {
        if let Some(index) = text.find(marker) {
            end = end.min(index);
        }
    }
    &text[..end]
}

#[test]
fn production_has_no_pointer_ownership_graphs_or_maps() {
    let src = root().join("src");
    let mut violations = Vec::new();
    for entry in fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let production = production_prefix(&text);
        for banned in [
            "Box<",
            "Box::",
            "Arc<",
            "Arc::",
            "Rc<",
            "Rc::",
            "Weak<",
            "Weak::",
            "dyn ",
            "HashMap<",
            "BTreeMap<",
        ] {
            if production.contains(banned) {
                violations.push(format!(
                    "{}:{banned}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "non-DOD production storage/ownership: {violations:?}"
    );
}

#[test]
fn process_runtime_is_flat_soa_with_free_passes() {
    let process = fs::read_to_string(root().join("src/process.rs")).unwrap();
    let production = production_prefix(&process);
    assert!(!production.contains("pub struct ProcessTable"));
    assert!(!production.contains("impl ProcessTable"));
    assert!(!production.contains("impl ProcessState"));
    assert!(!production.contains("impl ProcessArrays"));
    for field in [
        "process_id:",
        "process_command:",
        "process_pgid:",
        "process_time_ns:",
        "process_result:",
    ] {
        assert!(
            production.contains(field),
            "missing process SoA field: {field}"
        );
    }
    for pass in [
        "fn process_is_running(",
        "fn process_slot(",
        "fn reserve_process_slot(",
        "fn complete_process(",
    ] {
        assert!(
            production.contains(pass),
            "missing explicit process pass: {pass}"
        );
    }
}

#[test]
fn config_is_parallel_array_soa() {
    let config = fs::read_to_string(root().join("src/config.rs")).unwrap();
    let production = production_prefix(&config);
    for field in [
        "pub command_name: Vec<String>",
        "pub command_executable: Vec<PathBuf>",
        "pub env_name: Vec<String>",
        "pub env_value: Vec<OsString>",
        "pub command_path_grant: Vec<PathBuf>",
    ] {
        assert!(production.contains(field), "missing SoA field: {field}");
    }
    assert!(!production.contains("struct CommandEntry"));
    assert!(!production.contains("BTreeMap"));
    assert!(!production.contains("impl Config"));
    assert!(!production.contains("impl CommandId"));
    for pass in [
        "pub fn build_config(",
        "pub fn command_id(",
        "pub fn command_name(",
        "pub fn command_executable(",
        "pub fn root_for<",
    ] {
        assert!(
            production.contains(pass),
            "missing explicit config pass: {pass}"
        );
    }
}

#[test]
fn session_builds_parallel_command_arrays_directly() {
    let session = fs::read_to_string(root().join("src/session.rs")).unwrap();
    let production = production_prefix(&session);
    assert!(!production.contains("BTreeMap"));
    assert!(production.contains("command_name"));
    assert!(production.contains("command_executable"));
}

#[test]
fn context_core_stays_flat_and_mcp_free() {
    let context = fs::read_to_string(root().join("src/context.rs")).unwrap();
    let production = production_prefix(&context);
    for banned in [
        "serde",
        "serde_json",
        "schemars",
        "rmcp",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "struct Record",
        "ContextService",
        "Repository",
        "SegmentManager",
        "RetentionService",
    ] {
        assert!(
            !production.contains(banned),
            "context core gained unnecessary structure/dependency: {banned}"
        );
    }
    for required in [
        "const INDEX_ENTRY_BYTES: u64 = 16",
        "const MAX_SEGMENT_BYTES: u64 = 1024 * 1024",
        "pub(crate) fn read(",
        "pub(crate) fn checkpoint(",
        "pub(crate) fn prune(",
    ] {
        assert!(
            production.contains(required),
            "context core lost required flat primitive: {required}"
        );
    }
}
