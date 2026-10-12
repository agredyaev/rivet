#!/usr/bin/env python3
"""Run a real MCP stdio session against a built Rivet binary."""
import hashlib
import json
import os
import select
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CARGO_VERSION = next(
    line.split('"')[1]
    for line in (Path(__file__).parent.parent / "Cargo.toml").read_text().splitlines()
    if line.startswith('version = "')
)


def receive(proc):
    ready, _, _ = select.select([proc.stdout], [], [], 10)
    assert ready, "MCP response timed out"
    line = proc.stdout.readline()
    assert line, proc.stderr.read().decode()
    return json.loads(line)


def send(proc, number, method, params):
    proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": number, "method": method, "params": params}) + "\n").encode())
    proc.stdin.flush()
    response = receive(proc)
    assert response["id"] == number, response
    assert "error" not in response, response
    return response["result"]


def call(proc, number, name, arguments, error=None):
    result = send(proc, number, "tools/call", {"name": name, "arguments": arguments})
    if error:
        assert result["isError"] and result["structuredContent"]["code"] == error, result
    else:
        assert not result.get("isError"), result
    return result["structuredContent"]


_wait_call_id = 900000

def process_row(snapshot, process_id):
    for row in snapshot["running"]:
        if row["process_id"] == process_id:
            return "running", row
    for row in snapshot["completed"]:
        if row["process_id"] == process_id:
            return "completed", row
    raise AssertionError(f"process {process_id} missing from snapshot: {snapshot}")

def run_to_completion(proc, number, arguments):
    global _wait_call_id
    submitted = call(proc, number, "run_command", arguments)
    process_id = submitted["process_id"]
    for _ in range(120):
        _wait_call_id += 1
        snapshot = call(proc, _wait_call_id, "list_processes", {})
        state, row = process_row(snapshot, process_id)
        if state == "completed":
            _wait_call_id += 1
            output = call(proc, _wait_call_id, "read_process_output", {"process_id": process_id, "limit": 64})
            return {
                "process_id": process_id,
                "exit_code": row["exit_code"],
                "timed_out": row["timed_out"],
                "stopped": row["stopped"],
                "stdout": output["stdout"]["text"],
                "stderr": output["stderr"]["text"],
                "truncated": output["stdout"]["truncated"] or output["stderr"]["truncated"],
            }
        time.sleep(.05)
    raise AssertionError("submitted run_command did not complete")


def assert_server_info(result):
    assert result["serverInfo"] == {
        "name": "rivet",
        "title": "Rivet",
        "version": CARGO_VERSION,
        "description": "Local MCP server for configured commands and allowed filesystem roots.",
        "websiteUrl": "https://github.com/agredyaev/rivet",
    }, result
    instructions = result["instructions"]
    assert "Batch independent operations." in instructions
    assert "Submit long-running commands asynchronously" in instructions
    assert "Use list_processes for process state" in instructions
    assert "read_process_output only when captured output is required" in instructions
    assert "bounded phases" in instructions


OUTPUT_CAP = 2 * 1024 * 1024
FILE_CAP = 4 * 1024 * 1024
RUNNING_CAP = 8


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        scope = ["--root", tmp,
                 "--allow-command", "echo=/bin/echo", "--allow-any-args", "echo",
                 "--allow-command", "cat=/bin/cat", "--allow-any-args", "cat",
                 "--allow-command", "sh=/bin/sh", "--allow-subcommand", "sh=-c",
                 "--allow-command", "env=/usr/bin/env", "--allow-any-args", "env"]
        for action in ("doctor", "commands"):
            assert subprocess.run([binary, action, *scope], capture_output=True).returncode == 0
        assert subprocess.run([binary, "doctor", "--root", tmp], capture_output=True).returncode == 0
        assert subprocess.run([binary, "doctor", "--root", tmp,
                              "--allow-command", "missing=/missing-rivet-executable",
                              "--allow-any-args", "missing"], capture_output=True).returncode != 0
        assert subprocess.run([binary, "doctor", "--root", tmp,
                              "--allow-command", "echo=/bin/echo", "--allow-any-args", "echo",
                              "--allow-subcommand", "echo=x"], capture_output=True).returncode != 0
        workspace = root / "workspace"
        other_root = root / "other"
        workspace.mkdir()
        other_root.mkdir()
        (workspace / "one.txt").write_text("one")
        (other_root / "two.txt").write_text("two")
        assert subprocess.run([binary, "doctor"], capture_output=True).returncode != 0
        roots_and_commands = ["--root", ".", "--root", str(other_root), *scope[2:]]
        assert subprocess.run([binary, "doctor", *roots_and_commands], cwd=workspace, capture_output=True).returncode == 0
        scoped = subprocess.Popen([binary, "serve", *roots_and_commands], cwd=workspace, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            assert_server_info(send(scoped, 1, "initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}}))
            assert set(call(scoped, 101, "list_roots", {})["roots"]) == {str(workspace.resolve()), str(other_root.resolve())}
            assert call(scoped, 2, "read_file", {"path": str(workspace.resolve() / "one.txt")})["text"] == "one"
            assert call(scoped, 3, "read_file", {"path": str(other_root / "two.txt")})["text"] == "two"
            call(scoped, 4, "read_file", {"path": str(root / "outside.txt")}, "PATH_DENIED")
            call(scoped, 5, "run_command", {"command": "echo", "args": ["ok"], "cwd": tmp}, "PATH_DENIED")
        finally:
            scoped.stdin.close()
            assert scoped.wait(timeout=10) == 0
        (root / "rivet.toml").write_text("invalid TOML\n")
        server_env = {**os.environ, "HOME": "rivet-test-home", "RIVET_TEST_SECRET": "must-not-leak"}
        proc = subprocess.Popen([binary, "serve", *scope], cwd=root, env=server_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            assert_server_info(send(proc, 1, "initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}}))
            proc.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            proc.stdin.flush()
            tools = send(proc, 2, "tools/list", {})["tools"]
            assert {tool["name"] for tool in tools} == {"list_roots", "list_commands", "run_command", "run_commands", "start_process", "list_processes", "read_process_output", "send_process_input", "stop_process", "list_directory", "read_file", "read_files", "write_file", "replace_text"}
            assert all(tool["inputSchema"]["type"] == "object" for tool in tools)
            assert call(proc, 102, "list_roots", {})["roots"] == [str(root.resolve())]
            names = call(proc, 3, "list_commands", {})["commands"]
            assert [entry["name"] for entry in names] == ["cat", "echo", "env", "sh"]
            base = {"command": "echo", "args": [";", "&&", "|", ">", "$(date)", "`date`"], "cwd": tmp}
            completed = run_to_completion(proc, 4, base)
            assert completed["stdout"] == "; && | > $(date) `date`\n"
            call(proc, 5, "run_command", {**base, "command": "missing"}, "COMMAND_NOT_FOUND")
            call(proc, 6, "run_command", {**base, "command": "sh", "args": ["-x"]}, "SUBCOMMAND_DENIED")
            call(proc, 7, "run_command", {**base, "cwd": "/"}, "PATH_DENIED")
            call(proc, 8, "run_command", {**base, "env": {"SECRET": "x"}}, "COMMAND_DENIED")
            env = run_to_completion(proc, 9, {**base, "command": "sh", "args": ["-c", "printf '%s' \"$RUST_LOG\""], "env": {"RUST_LOG": "yes"}})
            assert env["stdout"] == "yes"
            inherited = run_to_completion(proc, 91, {**base, "command": "sh", "args": ["-c", "printf '%s|%s' \"$HOME\" \"$RIVET_TEST_SECRET\""]})
            assert inherited["stdout"] == "rivet-test-home|"
            for timeout_ms in (0, 1_800_001):
                call(proc, 92, "run_command", {**base, "timeout_ms": timeout_ms}, "INVALID_REQUEST")
            path = root / "file.txt"
            call(proc, 10, "write_file", {"path": str(path), "content": "héllo", "mode": "create"})
            assert call(proc, 11, "read_file", {"path": str(path), "offset": 0, "limit": 2})["text"] == "h"
            call(proc, 12, "read_file", {"path": str(path), "offset": 2, "limit": 2}, "INVALID_REQUEST")
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            call(proc, 13, "replace_text", {"path": str(path), "old": "hé", "new": "H", "expected_occurrences": 2}, "FILE_CHANGED")
            assert path.read_text() == "héllo"
            call(proc, 14, "write_file", {"path": str(path), "content": "bad", "mode": "overwrite", "expected_sha256": "0" * 64}, "FILE_CHANGED")
            call(proc, 15, "replace_text", {"path": str(path), "old": "hé", "new": "H", "expected_occurrences": 1, "expected_sha256": digest})
            assert path.read_text() == "Hllo"
            call(proc, 16, "write_file", {"path": str(path), "content": "!", "mode": "append"})
            assert path.read_text() == "Hllo!"
            call(proc, 17, "write_file", {"path": str(path), "content": "x", "mode": "create"}, "FILE_CHANGED")
            call(proc, 170, "write_file", {"path": str(path), "content": "x", "mode": "append", "expected_sha256": "bad"}, "INVALID_REQUEST")
            call(proc, 171, "read_file", {"path": str(path), "limit": FILE_CAP + 1}, "INVALID_REQUEST")
            call(proc, 172, "replace_text", {"path": str(path), "old": "", "new": "x", "expected_occurrences": 1}, "INVALID_REQUEST")
            assert path.read_text() == "Hllo!"
            large = root / "large.txt"
            large.write_text("a" * (FILE_CAP // 2))
            call(proc, 173, "replace_text", {"path": str(large), "old": "a", "new": "abc", "expected_occurrences": FILE_CAP // 2}, "OUTPUT_LIMIT")
            assert large.read_text() == "a" * (FILE_CAP // 2)
            outside = root.parent / (root.name + "-outside")
            call(proc, 18, "read_file", {"path": str(outside)}, "PATH_DENIED")
            call(proc, 180, "read_file", {"path": str(root / ".." / "outside")}, "PATH_DENIED")
            link = root / "escape"
            link.symlink_to(root.parent)
            call(proc, 19, "read_file", {"path": str(link / "anything")}, "PATH_DENIED")
            call(proc, 20, "write_file", {"path": str(link), "content": "x", "mode": "overwrite"}, "PATH_DENIED")
            for argument in (str(outside), f"--output={outside}", f"-o{outside}", f"../{outside.name}", str(link / "new.txt")):
                call(proc, 200, "run_command", {"command": "echo", "args": [argument], "cwd": tmp}, "PATH_DENIED")
            call(proc, 2001, "start_process", {"command": "cat", "args": [str(outside)], "cwd": tmp}, "PATH_DENIED")
            inside_link = root / "inside"
            inside_link.symlink_to(path.name)
            assert call(proc, 201, "read_file", {"path": str(inside_link)})["text"] == "Hllo!"
            assert run_to_completion(proc, 202, {"command": "echo", "args": [str(inside_link)], "cwd": tmp})["exit_code"] == 0
            for i in range(5): (root / f"item{i}").touch()
            assert call(proc, 21, "list_directory", {"path": tmp, "max_entries": 4})["truncated"]
            output = run_to_completion(proc, 22, {"command": "sh", "args": ["-c", f"printf '%0{OUTPUT_CAP + 1}d' 0; printf '%0{OUTPUT_CAP + 1}d' 0 >&2"], "cwd": tmp})
            assert output["truncated"] and len(output["stdout"]) == len(output["stderr"]) == 64
            assert run_to_completion(proc, 220, {"command": "sh", "args": ["-c", "exit 7"], "cwd": tmp})["exit_code"] == 7
            timeout = run_to_completion(proc, 23, {"command": "sh", "args": ["-c", "echo ready; sleep 3"], "cwd": tmp, "timeout_ms": 50})
            assert timeout["timed_out"] and "ready" in timeout["stdout"]
            assert run_to_completion(proc, 231, {"command": "sh", "args": ["-c", "sleep 3 &"], "cwd": tmp, "timeout_ms": 50})["timed_out"]
            detached_id = call(proc, 232, "start_process", {"command": "sh", "args": ["-c", "sleep .4; printf detached"], "cwd": tmp, "timeout_ms": 1000})["process_id"]
            snapshot = call(proc, 233, "list_processes", {})
            state, row = process_row(snapshot, detached_id)
            assert row["command"] == "sh"
            for _ in range(30):
                snapshot = call(proc, 234, "list_processes", {})
                state, row = process_row(snapshot, detached_id)
                if state == "completed": break
                time.sleep(.05)
            else: raise AssertionError("detached process did not complete")
            detached_output = call(proc, 235, "read_process_output", {"process_id": detached_id, "limit": 64})
            assert detached_output["stdout"]["text"] == "detached"
            assert detached_output["stderr"]["text"] == ""

            waited_id = call(proc, 238, "start_process", {"command": "sh", "args": ["-c", "sleep .2; printf waited"], "cwd": tmp, "timeout_ms": 1000})["process_id"]
            for _ in range(30):
                snapshot = call(proc, 239, "list_processes", {})
                state, row = process_row(snapshot, waited_id)
                if state == "completed": break
                time.sleep(.05)
            else: raise AssertionError("waited process did not complete")
            waited_output = call(proc, 240, "read_process_output", {"process_id": waited_id, "limit": 64})
            assert waited_output["stdout"]["text"] == "waited"

            started = call(proc, 24, "start_process", {"command": "cat", "args": [], "cwd": tmp})["process_id"]
            call(proc, 25, "send_process_input", {"process_id": started, "text": "hello\n"})
            for _ in range(20):
                read = call(proc, 26, "read_process_output", {"process_id": started, "stdout_offset": 0, "stderr_offset": 0, "limit": 64})
                if "hello" in read["stdout"]["text"]: break
                time.sleep(.05)
            else: raise AssertionError("process output missing")
            assert read["state"] == "running"
            assert read["stdout"]["next_offset"] == 6

            running = [started]
            for _ in range(RUNNING_CAP - 1):
                running.append(call(proc, 27, "start_process", {"command": "cat", "args": [], "cwd": tmp})["process_id"])
            snapshot = call(proc, 270, "list_processes", {})
            assert {row["process_id"] for row in snapshot["running"]} == set(running)
            call(proc, 271, "run_command", {"command": "echo", "args": ["full"], "cwd": tmp}, "PROCESS_LIMIT")
            call(proc, 272, "run_command", {"command": "sh", "args": ["-c", "sleep 2"], "cwd": tmp, "timeout_ms": 1000}, "PROCESS_LIMIT")
            call(proc, 28, "start_process", {"command": "cat", "args": [], "cwd": tmp}, "PROCESS_LIMIT")

            for process_id in running:
                assert call(proc, 29, "stop_process", {"process_id": process_id})["status"]["stopped"]

            third = call(proc, 31, "start_process", {"command": "echo", "args": ["done"], "cwd": tmp})["process_id"]
            for _ in range(20):
                snapshot = call(proc, 32, "list_processes", {})
                state, row = process_row(snapshot, third)
                if state == "completed": break
                time.sleep(.05)
            else: raise AssertionError("normal exit missing")
            assert row["exit_code"] == 0
            read = call(proc, 321, "read_process_output", {"process_id": third, "limit": 64})
            assert read["state"] == "completed" and read["stdout"]["text"] == "done\n"
            snapshot = call(proc, 33, "list_processes", {})
            assert len(snapshot["running"]) + len(snapshot["completed"]) <= 32

            retained = []
            for i in range(35):
                retained.append(
                    run_to_completion(
                        proc,
                        5000 + i,
                        {"command": "echo", "args": [str(i)], "cwd": tmp},
                    )["process_id"]
                )
            snapshot = call(proc, 5036, "list_processes", {})
            assert not snapshot["running"]
            assert [row["process_id"] for row in snapshot["completed"]] == retained[-32:]
            call(proc, 5037, "read_process_output", {"process_id": retained[0]}, "PROCESS_NOT_FOUND")

            timed = call(proc, 34, "start_process", {"command": "sh", "args": ["-c", "sleep 3"], "cwd": tmp, "timeout_ms": 50})["process_id"]
            for _ in range(40):
                snapshot = call(proc, 35, "list_processes", {})
                state, row = process_row(snapshot, timed)
                if state == "completed": break
                time.sleep(.05)
            else: raise AssertionError("managed timeout missing")
            assert row["timed_out"]

            truncated_id = call(proc, 350, "start_process", {"command": "sh", "args": ["-c", f"printf '%0{OUTPUT_CAP + 1}d' 0"], "cwd": tmp})["process_id"]
            for _ in range(20):
                snapshot = call(proc, 351, "list_processes", {})
                state, _ = process_row(snapshot, truncated_id)
                if state == "completed": break
                time.sleep(.05)
            read = call(proc, 352, "read_process_output", {"process_id": truncated_id, "limit": 64})
            assert read["stdout"]["truncated"] and read["stdout"]["next_offset"] == 64
            tail = call(proc, 353, "read_process_output", {"process_id": truncated_id, "stdout_offset": OUTPUT_CAP, "limit": 64})
            assert tail["stdout"]["text"] == "" and tail["stdout"]["next_offset"] == OUTPUT_CAP + 1
            assert read["stdout"]["total_bytes"] == OUTPUT_CAP + 1
            call(proc, 354, "read_process_output", {"process_id": truncated_id, "limit": OUTPUT_CAP + 1}, "INVALID_REQUEST")
            marker = root / "orphaned"
            script = f"(sleep 2; echo orphan > {marker}) & wait"
            group = call(proc, 36, "start_process", {"command": "sh", "args": ["-c", script], "cwd": tmp})["process_id"]
            time.sleep(.1)
            call(proc, 37, "stop_process", {"process_id": group})
            time.sleep(2.1)
            assert not marker.exists(), "stop left a child process running"
            shutdown_marker = root / "shutdown_orphan"
            script = f"(sleep 2; echo orphan > {shutdown_marker}) & wait"
            call(proc, 38, "start_process", {"command": "sh", "args": ["-c", script], "cwd": tmp})
            time.sleep(.1)
        finally:
            proc.stdin.close()
            proc.wait(timeout=10)
        time.sleep(2.1)
        assert not shutdown_marker.exists(), "server shutdown left a child process running"
        assert not proc.stdout.read(), "unexpected stdout"
        # An unterminated oversized line must close the transport without waiting for a newline.
        oversized = subprocess.Popen([binary, "serve", *scope], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        oversized.stdin.write(b"x" * (1024 * 1024 + 1024 * 6 + 2))
        oversized.stdin.flush()
        oversized.stdin.close()
        assert oversized.wait(timeout=10) != 0
    print("MCP smoke passed")


if __name__ == "__main__":
    main()
