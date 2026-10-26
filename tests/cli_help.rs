use std::process::Command;

#[test]
fn help_exposes_only_session_runtime() {
    let binary = env!("CARGO_BIN_EXE_rivet");

    let root = Command::new(binary).arg("--help").output().unwrap();
    assert!(root.status.success());
    let root_help = String::from_utf8_lossy(&root.stdout);
    assert!(root_help.contains("session"));
    assert!(!root_help.contains("\n  serve"));
    assert!(!root_help.contains("\n  doctor"));
    assert!(!root_help.contains("\n  commands"));

    let session = Command::new(binary)
        .args(["session", "--help"])
        .output()
        .unwrap();
    assert!(session.status.success());
    let session_help = String::from_utf8_lossy(&session.stdout);
    assert!(session_help.contains("--allow-command NAME=EXECUTABLE"));
    assert!(session_help.contains("--allow-command-path PATH"));
    assert!(session_help.contains("--pass-env NAME"));
    assert!(session_help.contains("--network"));
    assert!(!session_help.contains("--allow-subcommand"));
    assert!(!session_help.contains("--allow-any-args"));
    assert!(!session_help.contains("--root"));
}

#[test]
fn removed_runtime_commands_fail_closed() {
    let binary = env!("CARGO_BIN_EXE_rivet");
    for command in ["serve", "doctor", "commands"] {
        let output = Command::new(binary).arg(command).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&format!("unknown command: {command}"))
        );
    }
}

#[test]
fn version_identifies_binary() {
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
}
