#!/usr/bin/env python3
"""Measure release binary size, MCP initialize latency, and idle RSS/CPU on macOS."""
import argparse
import json
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--calls", type=int, default=101)
    parser.add_argument("--sample", type=Path, help="save a 5-second macOS CPU sample")
    args = parser.parse_args()
    if args.calls < 1:
        parser.error("--calls must be positive")
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory() as tmp:
        config = Path(tmp) / "rivet.toml"
        small_file = Path(tmp) / "small.txt"
        small_file.write_text("x" * 1024)
        config.write_text('''[limits]
max_stdout_bytes = 2097152
max_stderr_bytes = 2097152
max_file_read_bytes = 4194304
max_directory_entries = 1000
max_running_processes = 8
default_timeout_ms = 120000
max_timeout_ms = 1800000
[environment]
pass = ["PATH"]
allow_override = []
''')
        scope = ["--root", tmp, "--allow-command", "echo=/bin/echo", "--allow-any-args", "echo"]
        request = (json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "measure", "version": "1"}}}) + "\n").encode()
        samples = []
        for _ in range(21):
            start = time.perf_counter_ns()
            proc = subprocess.Popen([binary, "serve", "--config", config, *scope], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            proc.stdin.write(request)
            proc.stdin.flush()
            response = json.loads(proc.stdout.readline())
            assert response["id"] == 1 and "result" in response
            samples.append((time.perf_counter_ns() - start) / 1_000_000)
            proc.stdin.close()
            assert proc.wait(timeout=10) == 0
        proc = subprocess.Popen([binary, "serve", "--config", config, *scope], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        proc.stdin.write(request)
        proc.stdin.flush()
        assert json.loads(proc.stdout.readline())["id"] == 1
        time.sleep(2)
        def process_stats():
            try:
                return subprocess.check_output(
                    ["ps", "-o", "rss=,%cpu=", "-p", str(proc.pid)],
                    text=True,
                    stderr=subprocess.DEVNULL,
                ).strip()
            except (OSError, subprocess.CalledProcessError) as error:
                return f"unavailable ({type(error).__name__}: {error})"

        ps = process_stats()
        def tool(number, name, arguments):
            request = {"jsonrpc": "2.0", "id": number, "method": "tools/call", "params": {"name": name, "arguments": arguments}}
            start = time.perf_counter_ns()
            proc.stdin.write((json.dumps(request) + "\n").encode())
            proc.stdin.flush()
            reply = json.loads(proc.stdout.readline())
            assert reply["id"] == number and not reply["result"].get("isError"), reply
            return (time.perf_counter_ns() - start) / 1_000_000
        profile = subprocess.Popen(["sample", str(proc.pid), "5", "-file", str(args.sample.resolve())], stdout=subprocess.DEVNULL) if args.sample else None
        reads_start = time.perf_counter_ns()
        reads = [tool(100 + i, "read_file", {"path": str(small_file)}) for i in range(args.calls)]
        reads_wall_ms = (time.perf_counter_ns() - reads_start) / 1_000_000
        after_reads = process_stats()
        if profile:
            assert profile.wait(timeout=15) == 0
        commands = [tool(300 + i, "run_command", {"command": "echo", "args": ["ok"], "cwd": tmp}) for i in range(21)]
        proc.stdin.close()
        assert proc.wait(timeout=10) == 0
        print(json.dumps({
            "binary_bytes": binary.stat().st_size,
            "startup_initialize_median_ms": round(statistics.median(samples), 2),
            "startup_initialize_min_ms": round(min(samples), 2),
            "startup_initialize_max_ms": round(max(samples), 2),
            "idle_rss_kib_and_cpu_percent": ps,
            "read_file_calls": args.calls,
            "rss_kib_and_cpu_percent_after_reads": after_reads,
            "read_file_total_wall_ms": round(reads_wall_ms, 3),
            "read_file_median_ms": round(statistics.median(reads), 3),
            "run_command_submit_echo_median_ms_21_calls": round(statistics.median(commands), 3),
        }, indent=2))


if __name__ == "__main__":
    main()
