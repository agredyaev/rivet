# Performance measurements

Performance numbers are not checked in as release guarantees. Rivet's latency and memory use depend on the host, toolchain, filesystem, sandbox backend, and build profile. Measure the exact binary you plan to compare.

## Release build

Build the current source and run the measurement harness:

```sh
cargo build --locked --release
python3 tests/measure.py target/release/rivet --calls 100000
```

The harness reports JSON containing:

- release binary size;
- MCP initialize latency across 21 fresh server starts;
- idle RSS and CPU when the host permits process inspection;
- total and median latency for sequential 1 KiB `read_file` calls;
- RSS and CPU after the read workload;
- median `run_command` submission latency across 21 commands.

`run_command` latency measures submission only. The harness waits for each submitted process to complete before starting the next measured submission, so the process-capacity limit does not contaminate the result.

## Profiling build

The `profiling` Cargo profile keeps release optimization and debug symbols:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```

On macOS, `--sample PATH` records a five-second CPU sample with the system `sample` command. CPU sampling changes timing, so use an unsampled run for elapsed-time comparisons.

## Comparison rules

For an A/B comparison:

1. use the same machine, OS version, power state, filesystem, workspace, and command scope;
2. build both revisions with the same Rust toolchain and Cargo profile;
3. run the same call count and input files;
4. compare unsampled wall-clock measurements;
5. record the exact Git commit and command line beside any published result.

If RSS or CPU inspection is blocked by the host, the harness reports that metric as unavailable instead of substituting an estimate.
