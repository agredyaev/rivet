# Development

## Local checks

Install Rust with Edition 2024 support and Python 3. From the repository root:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 tests/smoke.py target/release/rivet
```

On Windows, CI uses `tests/portable_smoke.py` with the native Windows executable.

## Cross-compilation

CI installs `cargo-zigbuild` and builds Linux x64 and Windows x64 GNU targets on Ubuntu:

```sh
python3 -m pip install cargo-zigbuild==0.22.1
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-gnu
cargo zigbuild --locked --release \
  --target x86_64-unknown-linux-gnu \
  --target x86_64-pc-windows-gnu
```

The CI workflow runs native builds and smoke tests on macOS, Linux, and Windows. It runs the cross-built Linux binary on Ubuntu and Windows executable on Windows, then uploads both as `rivet-cross-x64`.

## Profiling

The `profiling` Cargo profile keeps release optimization and debug symbols. On macOS, collect a CPU sample while measuring the MCP workload:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```

The measurement script prints JSON with startup latency, idle RSS/CPU, total wall-clock time, and per-call medians. CPU sampling adds overhead; compare unsampled runs on the same host when measuring elapsed time. See [measurements](measurements.md) for the recorded environment and results.
