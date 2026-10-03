use std::{fs, process::Command, time::SystemTime};

#[test]
fn help_does_not_load_the_default_config() {
    let directory = std::env::temp_dir().join(format!(
        "rivet-cli-help-{}-{:?}",
        std::process::id(),
        SystemTime::now()
    ));
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
