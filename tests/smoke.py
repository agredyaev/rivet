#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import sys
import tempfile

binary = str(Path(sys.argv[1]).resolve())

root_help = subprocess.run([binary, "--help"], text=True, capture_output=True, check=True).stdout
assert "\n  session" in root_help
for removed in ("serve", "doctor", "commands"):
    assert f"\n  {removed}" not in root_help
    result = subprocess.run([binary, removed], text=True, capture_output=True)
    assert result.returncode != 0
    assert f"unknown command: {removed}" in result.stderr

session_help = subprocess.run(
    [binary, "session", "--help"], text=True, capture_output=True, check=True
).stdout
assert "--allow-command NAME=EXECUTABLE" in session_help
assert "--pass-env NAME" in session_help
assert "--network" in session_help
assert "--allow-subcommand" not in session_help
assert "--allow-any-args" not in session_help
assert "--root" not in session_help

with tempfile.TemporaryDirectory() as workspace:
    env = dict(os.environ)
    missing = f"RIVET_TEST_MISSING_{os.getpid()}"
    env.pop(missing, None)
    result = subprocess.run(
        [
            binary,
            "session",
            "--allow-command",
            "echo=/bin/echo",
            "--pass-env",
            missing,
        ],
        cwd=workspace,
        env=env,
        text=True,
        capture_output=True,
    )
    assert result.returncode != 0
    assert "STARTUP_ERROR:" in result.stderr
    assert missing in result.stderr
    assert "not set" in result.stderr

    result = subprocess.run(
        [
            binary,
            "session",
            "--allow-command",
            "echo=/bin/echo",
            "--allow-subcommand",
            "echo=x",
        ],
        cwd=workspace,
        text=True,
        capture_output=True,
    )
    assert result.returncode != 0
    assert "unknown argument: --allow-subcommand" in result.stderr

print("CLI smoke: single session runtime and explicit environment admission passed")
