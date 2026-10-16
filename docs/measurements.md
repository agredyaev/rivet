# Performance measurements

Measure the production path: `rivet session -> Secure MCP Tunnel -> MCP dispatch -> native sandbox`. The removed stdio execution mode is not a valid benchmark surrogate.

Record the host, OS version, Rust toolchain, filesystem, workspace, command capabilities, network policy, environment capabilities, build profile, and Git commit with every result.

## Builds

Release:

```sh
cargo build --locked --release
```

Profiling build:

```sh
cargo build --locked --profile profiling
```

The `profiling` Cargo profile keeps release optimization and debug symbols.

## Rivet profiling events

Set `RIVET_PROFILE_FILE` on the Rivet process and run a normal session:

```sh
RIVET_PROFILE_FILE=rivet-profile.jsonl \
target/profiling/rivet session \
  --allow-command cargo=cargo
```

Then execute the same MCP workload through the tunnel for every candidate. Rivet profiling events expose dispatch and process metrics without creating a second execution architecture for measurement.

For command-start measurements, include native sandbox construction and spawn cost. For end-to-end measurements, include tunnel round trips and the actual MCP workload.

## Comparison rules

For an A/B comparison:

1. use the same machine, OS version, power state, filesystem, workspace, and capability set;
2. build both revisions with the same Rust toolchain and Cargo profile;
3. execute the same MCP requests and child commands;
4. compare the same lifecycle points and units;
5. distinguish local dispatch/process metrics from end-to-end tunnel latency;
6. record the exact Git commit and command line beside any published result.

Do not use a local transport or unsandboxed child path to claim production-session performance.
