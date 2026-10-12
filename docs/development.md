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

## Fuzzing

The untrusted tunnel wire boundary is fuzzed with `fuzz/fuzz_targets/tunnel_wire.rs`:

```sh
cargo +nightly fuzz run tunnel_wire -- -max_total_time=10 -timeout=2
```

CI runs the same 10-second bounded fuzz target before release artifacts are packaged. Fuzz build output, corpus, and crash artifacts are generated locally and are not committed.

## Clippy

CI runs Clippy for all targets with `-D warnings`. The checked-in source has no `#[allow(clippy::...)]` attributes. Keep any future suppression on the smallest affected item and put its reason beside the attribute; do not add a crate-wide suppression.

## Cross-compilation

CI installs `cargo-zigbuild` and builds Linux x86_64 and Linux aarch64 targets on Ubuntu:

```sh
python3 -m pip install cargo-zigbuild==0.23.4
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
cargo zigbuild --locked --release \
  --target x86_64-unknown-linux-gnu \
  --target aarch64-unknown-linux-gnu
```

The CI workflow runs native builds and smoke tests on macOS and Linux, plus cross-builds Linux x86_64/aarch64. Published GitHub releases contain `rivet-linux-x64.zip`, `rivet-linux-aarch64.zip`, and `rivet-darwin-arm64.zip`; Windows users run the Linux build under WSL2.

## Releases

Use `fix:`, `perf:`, or `refactor:` for a patch release, `feat:` for a minor release, and `!` or `BREAKING CHANGE:` for a breaking release. After changes reach `main`, release-please opens a release PR and updates `Cargo.toml`, `Cargo.lock`, the version manifest, and `CHANGELOG.md`. Merge that PR to create the GitHub Release. Do not edit the package version by hand.

CI builds, tests, fuzzes, cross-builds, verifies binary architectures, and packages the release artifacts before publication. The release workflow requires the tag to equal `v<Cargo.toml version>` and requires a successful CI run whose `head_sha` exactly matches the release tag commit. It downloads that run's `rivet-release-assets` artifact, rejects any file outside the three supported platform ZIPs plus `SHA256SUMS`, verifies the checksums, and uploads those exact files to the GitHub Release. It does not rebuild release binaries. To republish assets for an existing release, run the `Release` workflow manually with that tag.

Release Please uses `GITHUB_TOKEN`, so its generated release PR does not start a separate `pull_request` workflow. Source changes must pass CI before they reach `main`.

## Profiling

The `profiling` Cargo profile keeps release optimization and debug symbols. On macOS, collect a CPU sample while measuring the MCP workload:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```

The measurement script prints JSON with startup latency, idle RSS/CPU when the host permits process inspection, total wall-clock time, and per-call medians. Restricted environments report RSS/CPU as unavailable. CPU sampling adds overhead; compare unsampled runs on the same host when measuring elapsed time. See the [measurement procedure](measurements.md) for the exact workload and comparison rules.
