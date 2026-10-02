# Release measurements

Measurements below were collected on 2026-10-02 from a macOS arm64 host (Apple M2, macOS 27.0, Rust 1.98.1). They describe one machine and are not performance guarantees for other systems.

| Measure | Release build | Profiling build |
| --- | ---: | ---: |
| Binary size | 1,434,096 bytes | 2,166,424 bytes |
| MCP startup median | 2.75 ms | 2.80 ms |
| Idle RSS after 2 seconds | 2,944 KiB | 2,896 KiB |
| Idle CPU | 0.0% | 0.0% |
| 100,000 sequential 1 KiB reads, wall-clock | 8.485 s | 9.228 s |
| Median read call | 0.059 ms | 0.062 ms |
| RSS after reads | 4,016 KiB | 3,872 KiB |

The read workload includes the Python client, JSON encoding, stdio transport, and server. The profiling run collected a five-second CPU sample; sampling and host load affect timings. The sample includes Rust symbols and source locations.

The measurement script also records command-call latency and emits JSON. Run it without sampling for elapsed-time comparisons:

```sh
cargo build --locked --release
python3 tests/measure.py target/release/rivet --calls 100000
```

For a symbolized CPU sample:

```sh
cargo build --locked --profile profiling
python3 tests/measure.py target/profiling/rivet --calls 100000 --sample rivet-profile.txt
```
