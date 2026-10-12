use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn help_ignores_legacy_config_file() {
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
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rivet"))
            .args(&args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage: rivet"));
        assert!(!stdout.contains("--config"));
        assert!(!stdout.contains("config-check"));
        if args.first() == Some(&"session") {
            assert!(stdout.contains("CONTROL_PLANE_TUNNEL_ID"));
            assert!(stdout.contains("CONTROL_PLANE_API_KEY"));
            assert!(stdout.contains("terminal echo disabled"));
        }
    }

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn standalone_binary_starts_without_config_and_ignores_legacy_files() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("rivet-standalone-{}-{nonce}", std::process::id()));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let binary = root.join("rivet");
    fs::copy(env!("CARGO_BIN_EXE_rivet"), &binary).unwrap();

    for legacy_file in [false, true] {
        if legacy_file {
            for directory in [&root, &workspace] {
                fs::write(directory.join("rivet.toml"), "invalid TOML\n").unwrap();
            }
        }
        for action in ["doctor", "commands"] {
            let output = Command::new(&binary)
                .args([action, "--root", "."])
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    for (args, message) in [
        (
            vec!["doctor", "--config", "missing.toml"],
            "unknown argument: --config",
        ),
        (
            vec!["config-check", "--help"],
            "unknown command: config-check",
        ),
        (vec!["doctor"], "at least one --root is required"),
        (vec!["doctor", "--root", "missing"], "STARTUP_ERROR"),
        (
            vec!["doctor", "--root", ".", "--root", "."],
            "duplicate root",
        ),
    ] {
        let output = Command::new(&binary)
            .args(args)
            .current_dir(&workspace)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn version_identifies_binary_and_help_documents_repeated_roots() {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_rivet"))
            .arg(flag)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("rivet {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_rivet"))
        .args(["doctor", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("--root PATH [--root PATH]..."));
}
