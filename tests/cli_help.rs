use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn help_does_not_load_the_default_config() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("rivet-cli-help-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("rivet.toml"), "[unsupported]\n").unwrap();

    for args in [
        vec!["--help"],
        vec!["serve", "--help"],
        vec!["session", "--help"],
        vec!["doctor", "--help"],
        vec!["commands", "-h"],
        vec!["config-check", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rivet"))
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: rivet"));
    }

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn example_config_is_valid() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rivet-example-config-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let config = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rivet.example.toml");

    let output = Command::new(env!("CARGO_BIN_EXE_rivet"))
        .arg("config-check")
        .arg("--config")
        .arg(&config)
        .arg("--root")
        .arg(&root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
