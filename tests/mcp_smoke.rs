#[test]
fn real_stdio_session() {
    let (python, script) = if cfg!(windows) {
        ("python", "/tests/portable_smoke.py")
    } else {
        ("python3", "/tests/smoke.py")
    };
    let output = std::process::Command::new(python)
        .arg(format!("{}{script}", env!("CARGO_MANIFEST_DIR")))
        .arg(env!("CARGO_BIN_EXE_rivet"))
        .output()
        .expect("run MCP smoke check");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
