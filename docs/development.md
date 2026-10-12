# Development

## Local checks

Install the current stable Rust toolchain and Python 3. From the repository root:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 tests/smoke.py target/release/rivet
python3 tests/install_smoke.py
```

## Dependency checks

Install audit tools separately from Rivet's dependencies:

```sh
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-machete --version 0.9.2 --locked
cargo deny --locked check
cargo deny --locked --manifest-path fuzz/Cargo.toml --config deny.toml check
cargo machete
python3 tests/dependencies.py
```

`deny.toml` is a development policy, not a Rivet startup config. CI checks
RustSec advisories, allowed licenses, dependency versions and sources for both
production and fuzzing. Multiple transitive versions produce warnings; advisory
exceptions are empty. All project and third-party licenses are checked, with
NCSA allowed only for libfuzzer-sys. Rivet and its fuzz harness are licensed under MIT.

The dependency check follows normal edges for all three release targets and
stops at procedural macros. Build tools, fuzzing, audit tools, and the removed
TOML parser must stay outside that runtime graph. This graph check does not
measure which runtime functions survive LTO. Inspect final dynamic dependencies
with `otool -L` on macOS and `readelf -d` on Linux; statically linked libraries
will not appear in those lists.

## Fuzzing

Release CI fuzzes the critical untrusted-input and bounded-state boundaries with seven production-backed targets:

- `tunnel_wire`: malformed tunnel poll/command JSON, timeout parsing, JSON-RPC IDs, and tunnel IDs. A committed seed corpus and dictionary drive deep parser paths.
- `tunnel_wire_structured`: valid generated tunnel commands with semantic invariants for correlation fields, command types, timeouts, future fields, JSON-RPC IDs, and tunnel IDs.
- `tunnel_policy`: terminal/retry/auth HTTP classification and bounded monotonic retry backoff.
- `mcp_wire`: MCP JSON-RPC decoding and stable request/notification/unsupported classification across serialize/parse round trips. A committed seed corpus and dictionary cover valid MCP shapes.
- `command_policy`: exact subcommand admission, NUL rejection, and timeout bounds.
- `path_policy`: workspace-relative path admission, parent traversal rejection, sibling-prefix rejection, and relative-path rejection.
- `output_buffer`: bounded process-output capture, reset, offset reads, truncation, and invalid-offset handling.

Run a target locally with nightly, for example:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --version 0.13.2 --locked
cargo +nightly fuzz run tunnel_wire -- -max_total_time=20 -timeout=2 -dict=fuzz/dictionaries/tunnel_wire.dict
cargo +nightly fuzz run mcp_wire -- -max_total_time=20 -timeout=2 -dict=fuzz/dictionaries/mcp_wire.dict
cargo +nightly fuzz run output_buffer -- -max_total_time=20 -timeout=2
```

CI runs every target for 10 seconds before release artifacts are packaged. Committed seed corpora live under `fuzz/corpus/`; generated corpus growth, build output, and crash artifacts remain untracked.

## Clippy

CI runs Clippy for all targets with `-D warnings`. The checked-in source has no `#[allow(clippy::...)]` attributes. Keep any future suppression on the smallest affected item and put its reason beside the attribute; do not add a crate-wide suppression.

## Cross-compilation

CI installs `cargo-zigbuild` and builds Linux x86_64 and Linux aarch64 targets on Ubuntu:

```sh
python3 -m pip install cargo-zigbuild==0.23.4 ziglang==0.17.0
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
export PATH="$(python3 -c 'import pathlib, ziglang; print(pathlib.Path(ziglang.__file__).parent)'):$PATH"
test "$(zig version)" = "0.17.0"
cargo zigbuild --locked --release \
  --target x86_64-unknown-linux-gnu \
  --target aarch64-unknown-linux-gnu
```

The CI workflow runs native builds and smoke tests on macOS and Linux, plus cross-builds Linux x86_64/aarch64. The release archive names are `rivet-v<VERSION>-linux-x64.zip`, `rivet-v<VERSION>-linux-aarch64.zip`, and `rivet-v<VERSION>-darwin-arm64.zip`; Windows users run the Linux build under WSL2.

With Zig installed through zvm, use `zvm install --nomirror 0.17.0` and prepend `"$HOME/.zvm/0.17.0"` to `PATH` instead of the Python package directory. Confirm `zig version` before building so an older system Zig cannot take precedence.

## Releases

Use `fix:`, `perf:`, or `refactor:` for a patch release, `feat:` for a minor release, and `!` or `BREAKING CHANGE:` for a breaking release. After changes reach `main`, release-please opens a release PR and updates `Cargo.toml`, `Cargo.lock`, the version manifest, and `CHANGELOG.md`. Merge that PR to create the GitHub Release. Do not edit the package version by hand.

CI audits dependencies, builds, tests, fuzzes, cross-builds, verifies binary architectures, and packages the release artifacts before publication. The release workflow requires the tag to equal `v<Cargo.toml version>` and requires a successful CI run whose `head_sha` exactly matches the release tag commit. It downloads that run's `rivet-release-assets` artifact, rejects any file outside the three supported platform ZIPs plus `SHA256SUMS`, verifies the checksums, and uploads those exact files to the GitHub Release. CI reads `<VERSION>` from Cargo.toml; publication checks it against the tag. Each ZIP contains `bin/rivet` and `LICENSE`. `SHA256SUMS` keeps its name and lists the versioned ZIPs. It does not rebuild release binaries. To republish assets for an existing release, run the `Release` workflow manually with that tag.

Release Please uses `GITHUB_TOKEN`, so its generated release PR does not start a separate `pull_request` workflow. Source changes must pass CI before they reach `main`.

CI ignores changes limited to Markdown or `docs/`. For a documentation-only release commit, run CI manually on that commit before publishing assets; the release gate still requires a successful run at the exact tag SHA.

## Profiling

The `profiling` Cargo profile keeps release optimization and debug symbols. On macOS, collect a CPU sample while measuring the MCP workload:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```

The measurement script prints JSON with startup latency, idle RSS/CPU when the host permits process inspection, total wall-clock time, and per-call medians. Restricted environments report RSS/CPU as unavailable. CPU sampling adds overhead; compare unsampled runs on the same host when measuring elapsed time. See the [measurement procedure](measurements.md) for the exact workload and comparison rules.
