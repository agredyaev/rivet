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

## Clippy

CI runs Clippy for all targets with `-D warnings`. The checked-in source has no `#[allow(clippy::...)]` attributes. Keep any future suppression on the smallest affected item and put its reason beside the attribute; do not add a crate-wide suppression.

## Cross-compilation

CI installs `cargo-zigbuild` and builds Linux x86_64, Linux aarch64, and Windows x86_64 GNU targets on Ubuntu:

```sh
python3 -m pip install cargo-zigbuild==0.23.4
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-gnu
cargo zigbuild --locked --release \
  --target x86_64-unknown-linux-gnu \
  --target aarch64-unknown-linux-gnu \
  --target x86_64-pc-windows-gnu
```

The CI workflow runs native builds and smoke tests on macOS, Linux, and Windows. It runs the cross-built Linux x64 binary on Ubuntu and Windows executable on Windows. Published GitHub releases contain `rivet-linux-x64.zip`, `rivet-linux-aarch64.zip`, `rivet-darwin-arm64.zip`, and `rivet-windows-x64.zip`; each archive contains the platform binary.

## Releases

Use `fix:` for a patch release, `feat:` for a minor release, and `!` or `BREAKING CHANGE:` for a breaking release. After changes reach `main`, release-please opens a release PR and updates `Cargo.toml`, `Cargo.lock`, the version manifest, and `CHANGELOG.md`. Merge that PR to create the GitHub Release; the same workflow then builds and uploads the platform archives and `SHA256SUMS`. Do not edit the package version by hand. To rebuild assets for an existing release, run the `Release` workflow manually with its tag.

Release Please uses `GITHUB_TOKEN`, so its generated release PR does not start a separate `pull_request` workflow. Source changes run the full CI before reaching `main`; the release workflow builds all release targets from the generated tag. If repository rules later require CI checks directly on release PRs, configure a repository secret token for the action.

## Profiling

The `profiling` Cargo profile keeps release optimization and debug symbols. On macOS, collect a CPU sample while measuring the MCP workload:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```

The measurement script prints JSON with startup latency, idle RSS/CPU when the host permits process inspection, total wall-clock time, and per-call medians. Restricted environments report RSS/CPU as unavailable. CPU sampling adds overhead; compare unsampled runs on the same host when measuring elapsed time. See [measurements](measurements.md) for the recorded environment and results.
