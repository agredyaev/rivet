# Rivet

[![CI](https://github.com/agredyaev/rivet/actions/workflows/ci.yml/badge.svg)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
![Rust 2024](https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white)
![Tokio](https://img.shields.io/badge/Tokio-async-purple?logo=tokio&logoColor=white)
![Zig](https://img.shields.io/badge/Zig-cross--compilation-f7a41d?logo=zig&logoColor=white)
![Platforms](https://img.shields.io/badge/OS-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)
![MCP stdio](https://img.shields.io/badge/MCP-stdio-blue)

Rivet is a local MCP server that gives an MCP client controlled access to
configured commands, files, and long-running processes. It communicates over
stdio and does not open a network listener. ChatGPT connectivity uses an
external tunnel client.

## Build and run

Rivet requires Rust with Edition 2024 support. Build targets are macOS, Linux,
and Windows.

```sh
git clone https://github.com/agredyaev/rivet.git
cd rivet
cargo build --release
cp rivet.example.toml rivet.toml
```

Edit `rivet.toml`: set `filesystem.allowed_roots` to existing absolute paths
that Rivet may access, and register the commands the client may run. Then
validate the configuration and start the server:

```sh
./target/release/rivet config-check
./target/release/rivet doctor
./target/release/rivet serve
```

By default, Rivet reads `./rivet.toml`. Pass `--config PATH` to use another
file. Repeat `--root PATH` to replace the configured roots for a session;
relative roots are resolved from the launch directory. For example:

```sh
./target/release/rivet serve --config /path/to/rivet.toml --root /path/to/project
```

Configure your MCP client to launch `rivet serve` as a stdio process. Keep
`rivet.toml` local: it contains machine-specific paths and command permissions.

## Tools

Rivet provides `list_roots`, `list_commands`, `run_command`, `start_process`,
`read_process_output`, `send_process_input`, `stop_process`, `list_directory`,
`read_file`, `write_file`, and `replace_text`.

Commands must be registered in the config. Each entry either allows arbitrary
arguments or restricts the first argument to an explicit subcommand list. File
operations and command working directories are restricted to the effective
allowed roots. Output, file reads, directory listings, process counts, and
runtime are bounded by config limits. See [rivet.example.toml](rivet.example.toml)
for the available settings and [docs/spec.md](docs/spec.md) for the full tool
and security contract.

Allowed roots constrain Rivet's file operations and working directories; they
are not an operating-system sandbox. Registered programs keep their normal
permissions. Register only commands and arguments you intend to expose.

## Checks

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python3 tests/smoke.py target/release/rivet
```

For measured release size and runtime data, see [docs/measurements.md](docs/measurements.md).

## Cross-compilation

Install [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), which uses
Zig as the linker, then build the Linux and Windows x64 binaries:

```sh
python3 -m pip install cargo-zigbuild==0.22.1
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-gnu
cargo zigbuild --locked --release --target x86_64-unknown-linux-gnu --target x86_64-pc-windows-gnu
```

CI runs native builds and MCP smoke tests on macOS, Linux, and Windows. It
also executes the Zig-built Linux and Windows binaries on their target OS.
The cross job uploads both release binaries as the `rivet-cross-x64` artifact.

## Profiling

The `profiling` Cargo profile keeps optimization and debug symbols. On macOS,
the measurement script can collect a CPU sample alongside wall-clock timings:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```

The JSON output includes MCP startup latency, idle RSS/CPU, total wall-clock
time for the read workload, and per-call medians. Sampling affects timings;
compare performance with an unsampled run on the same host.
