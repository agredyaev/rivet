#!/usr/bin/env python3
"""Exercise a real MCP session using Python workers on every supported OS."""
import json
import queue
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        worker = root / "worker.py"
        worker.write_text('''import subprocess, sys, time
from pathlib import Path
mode = sys.argv[1]
if mode == "echo": print("ready", flush=True)
elif mode == "input": print(sys.stdin.readline().strip(), flush=True)
elif mode == "marker":
    time.sleep(3)
    Path(sys.argv[2]).write_text("orphan")
elif mode == "tree":
    subprocess.Popen([sys.executable, __file__, "marker", sys.argv[2]])
    print("ready", flush=True)
    time.sleep(30)
''')
        config = root / "rivet.toml"
        config.write_text(f'''[filesystem]
allowed_roots = [{json.dumps(directory)}]
[limits]
max_stdout_bytes = 2048
max_stderr_bytes = 2048
max_file_read_bytes = 4096
max_directory_entries = 100
max_running_processes = 4
default_timeout_ms = 10000
max_timeout_ms = 30000
[environment]
pass = ["PATH", "SystemRoot", "WINDIR"]
allow_override = []
[commands.python]
executable = {json.dumps(sys.executable)}
allow_any_args = true
''')
        proc = subprocess.Popen([binary, "serve", "--config", config], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        replies = queue.Queue()
        def reader():
            for line in proc.stdout:
                replies.put(json.loads(line))
        threading.Thread(target=reader, daemon=True).start()
        number = 0
        def send(method, params):
            nonlocal number
            number += 1
            proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": number, "method": method, "params": params}) + "\n").encode())
            proc.stdin.flush()
            reply = replies.get(timeout=15)
            assert reply["id"] == number and "error" not in reply, reply
            return reply["result"]
        def call(name, arguments, error=None):
            result = send("tools/call", {"name": name, "arguments": arguments})
            value = result["structuredContent"]
            if error:
                assert result.get("isError") and value["code"] == error, result
            else:
                assert not result.get("isError"), result
            return value
        def command(mode, *args):
            return {"command": "python", "args": [str(worker), mode, *map(str, args)], "cwd": directory}
        def wait_output(process_id, predicate):
            for _ in range(100):
                value = call("read_process_output", {"process_id": process_id})
                if predicate(value):
                    return value
                time.sleep(.05)
            raise AssertionError("process did not reach the expected state")
        try:
            send("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "portable-smoke", "version": "1"}})
            assert len(send("tools/list", {})["tools"]) == 11
            path = root / "text.txt"
            call("write_file", {"path": str(path), "content": "hello", "mode": "create"})
            call("replace_text", {"path": str(path), "old": "hello", "new": "world", "expected_occurrences": 1})
            assert call("read_file", {"path": str(path)})["text"] == "world"
            call("read_file", {"path": str(root.parent / "outside.txt")}, "PATH_DENIED")
            assert call("run_command", command("echo"))["stdout"].strip() == "ready"
            process_id = call("start_process", command("input"))["process_id"]
            call("send_process_input", {"process_id": process_id, "text": "hello\n"})
            assert wait_output(process_id, lambda value: not value["status"]["running"])["stdout"]["text"].strip() == "hello"
            marker = root / "stopped.txt"
            process_id = call("start_process", command("tree", marker))["process_id"]
            wait_output(process_id, lambda value: "ready" in value["stdout"]["text"])
            assert call("stop_process", {"process_id": process_id})["status"]["stopped"]
            timeout_marker = root / "timeout.txt"
            process_id = call("start_process", {**command("tree", timeout_marker), "timeout_ms": 500})["process_id"]
            assert wait_output(process_id, lambda value: not value["status"]["running"])["status"]["timed_out"]
            time.sleep(3.2)
            assert not marker.exists() and not timeout_marker.exists(), "descendant survived stop or timeout"
        finally:
            proc.stdin.close()
            assert proc.wait(timeout=15) == 0, proc.stderr.read().decode()
    print("portable MCP smoke passed")


if __name__ == "__main__":
    main()
