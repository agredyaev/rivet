#[test]
fn real_stdio_session() {
    let output = std::process::Command::new("python3")
        .arg(format!("{}/tests/smoke.py", env!("CARGO_MANIFEST_DIR")))
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
