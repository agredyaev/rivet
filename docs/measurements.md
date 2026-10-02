# Rivet v0.1 release measurements

Measured 2026-09-29 on macOS 27.0, MacBook Air (Mac14,2), Apple M2,
Darwin arm64, Rust 1.98.1. The binary is a stripped Mach-O arm64 executable.

| Measure | Result | Spec target |
| --- | ---: | ---: |
| Binary size | 1,434,016 bytes (1.37 MiB) | <= 5 MiB |
| Idle RSS after MCP initialize and 2 seconds | 2,816–2,848 KiB | <= 15 MiB |
| Idle CPU from `ps` | 0.0% | effectively 0 |
| MCP initialize startup, median of 21 launches | 2.74–4.31 ms across two runs | <= 50 ms |

Paired follow-up (two runs per build, alternating order; each read median is 101 calls):

| Measure | Previous path copy | Borrowed path |
| --- | ---: | ---: |
| `read_file` 1 KiB, median wall time | 0.178–0.191 ms | 0.151–0.159 ms |
| RSS after 101 reads | 3,712–3,728 KiB | 3,648–3,680 KiB |

The change borrows the root-relative path instead of allocating a `PathBuf` on
each command or file request. A separate file-size-based buffer reserve was
reverted: paired reads were slower and RSS showed no consistent gain. Startup
and command-spawn times varied with machine load; this sample does not show a
reliable improvement in either. There is no graph or large dense process set
for a useful CSR or SoA layout in v0.1.

Further heap probe found two causes. The first `tools/list` or `tools/call`
initializes the MCP SDK's thread-local JSON schema caches: live 640-byte blocks
rise from 4 after `ping` to 95 after one tool request. The SDK macro also built
all ten tool routes on **every** tool request. Rivet now keeps one `ToolRouter`
in a `LazyLock`, removing that repeated allocation work while preserving lazy
initialization. After 1,000 `list_commands` calls, RSS was 3,600 KiB before
and 3,488 KiB after this change; live heap was 218 and 222 KiB respectively.
The extra live 4 KiB is the retained router. Most of the RSS increase beyond
live-heap growth came from resident `Malloc Small` pages retained after
temporary request allocations. `ping` alone
ended at 3,104–3,184 KiB after 1,000 calls.

In two paired 100,000-call runs, the old router took 7.76–14.83 seconds and
the retained router took 3.83–6.41 seconds; machine load varied substantially.
RSS after those runs was 3,776–3,888 KiB before and 3,760–3,840 KiB after.
The earlier 100,000-call probe without this change reached 3,952–4,000 KiB
and leveled off. In a separate 101-read timeline, Tokio's idle blocking worker
exited after 10 seconds while RSS stayed flat. On this Mac,
`MallocNanoZone=0` lowered RSS in that earlier probe to 3,520–3,536 KiB but
gave no RSS gain after 101 reads. One timed 100,000-call run took 6.48 seconds
versus 5.63 seconds with the default allocator. This setting is not enabled for
Rivet.

Commands:

```sh
cargo build --release
python3 tests/smoke.py target/release/rivet
python3 tests/measure.py target/release/rivet
stat -f '%z bytes' target/release/rivet
file target/release/rivet
sw_vers -productVersion
uname -sm
sysctl -n hw.model
system_profiler SPHardwareDataType | rg 'Chip:|Model Name:'
rustc --version
```

`measure.py` launches `rivet serve` with a temporary one-command configuration,
times from process creation through the initialize response, samples
`ps -o rss=,%cpu=` after two idle seconds and 101 small reads, and times
`read_file` and `run_command` calls. These are local measurements, not a
cross-machine performance guarantee.

## 2026-10-02 cross-build and sampled workload

The working tree was cross-compiled from macOS arm64 with Rust 1.98.1,
Zig 0.16.0, and cargo-zigbuild 0.22.1. Both release links succeeded:
`x86_64-unknown-linux-gnu` produced an ELF x86-64 executable and
`x86_64-pc-windows-gnu` produced a PE32+ x86-64 executable. These binaries
were not executed on their target operating systems in this run.

On the same macOS 27.0 / Apple M2 host, two runs each performed 100,000
sequential MCP `read_file` calls against a 1 KiB file while collecting a
five-second CPU sample. Wall-clock values include the Python client,
JSON serialization, pipe transport, and server work.

| Measure | Stripped release | Profiling build with symbols |
| --- | ---: | ---: |
| Binary bytes | 1,434,096 | 2,166,424 |
| Startup median | 2.75 ms | 2.80 ms |
| Startup min / max | 2.50 / 3.43 ms | 2.52 / 341.10 ms |
| Idle RSS / CPU after two seconds | 2,944 KiB / 0.0% | 2,896 KiB / 0.0% |
| 100,000 reads, total wall-clock | 8.485 s | 9.228 s |
| Per-read median | 0.059 ms | 0.062 ms |
| RSS after reads | 4,016 KiB | 3,872 KiB |
| Entire measurement harness, wall-clock | 10.71 s | 11.75 s |

The profiling build uses release optimization with debug symbols and no
stripping. Its sample includes Rust symbols and source locations. Sampling
and machine load affect timings; the startup outlier and the difference
between these runs do not establish a performance regression or improvement.

```sh
cargo zigbuild --locked --release --target x86_64-unknown-linux-gnu --target x86_64-pc-windows-gnu
cargo build --locked --profile profiling
/usr/bin/time -p python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```
