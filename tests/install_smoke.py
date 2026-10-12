#!/usr/bin/env python3
"""Exercise release asset selection and checksums without network requests."""

import errno
import fcntl
import hashlib
import os
from pathlib import Path
import pty
import subprocess
import sys
import tempfile
import termios
from zipfile import ZipFile


def executable(path, source):
    path.write_text(source)
    path.chmod(0o755)


def terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def main():
    installer = Path(__file__).resolve().parents[1] / "scripts/install.sh"
    cases = [
        ("versioned", "Darwin", "arm64", True),
        ("versioned", "Linux", "x86_64", True),
        ("versioned", "Linux", "aarch64", True),
        ("legacy", "Darwin", "arm64", True),
        ("missing", "Darwin", "arm64", False),
        ("bad-checksum", "Darwin", "arm64", False),
    ]
    for mode, platform, architecture, success in cases:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            binary = root / "rivet"
            executable(binary, '#!/bin/sh\nprintf "SESSION_STARTED\\n"\ntest "$1" = session\n')
            with ZipFile(root / "archive.zip", "w") as archive:
                archive.write(binary, "bin/rivet")
            digest = hashlib.sha256((root / "archive.zip").read_bytes()).hexdigest()
            executable(tools / "uname", '#!/bin/sh\ncase "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; *) exit 1;; esac\n')
            executable(tools / "bwrap", "#!/bin/sh\nexit 0\n")
            executable(tools / "curl", f"#!{sys.executable}\n" + '''
import os, pathlib, shutil, sys
args = sys.argv[1:]
url = next(argument for argument in args if argument.startswith("https://"))
root = pathlib.Path(os.environ["TEST_ROOT"])
mode = os.environ["TEST_MODE"]
if url.endswith(".release-please-manifest.json"):
    print('{".":"9.8.7"}')
    sys.exit(0)
assert "/releases/download/v9.8.7/" in url, url
asset = url.rsplit("/", 1)[-1]
if "-fsSIL" in args:
    expected = os.environ["TEST_ASSET"]
    if mode == "legacy":
        expected = expected.replace("-v9.8.7", "")
    present = mode != "missing" and asset == expected
    sys.exit(0 if present else 22)
destination = pathlib.Path(args[args.index("-o") + 1])
if asset == "SHA256SUMS":
    digest = "0" * 64 if mode == "bad-checksum" else os.environ["TEST_DIGEST"]
    names = ["rivet-v9.8.7-darwin-arm64.zip", "rivet-v9.8.7-linux-x64.zip",
             "rivet-v9.8.7-linux-aarch64.zip", "rivet-darwin-arm64.zip"]
    destination.write_text("".join(f"{digest}  {name}\\n" for name in names))
else:
    shutil.copyfile(root / "archive.zip", destination)
''')
            environment = {
                **os.environ, "PATH": f"{tools}:{os.environ['PATH']}",
                "RIVET_INSTALL_ROOT": str(root / "installed"), "NO_COLOR": "1",
                "TEST_ROOT": str(root), "TEST_MODE": mode, "TEST_DIGEST": digest,
                "TEST_OS": platform, "TEST_ARCH": architecture,
                "TEST_ASSET": "rivet-v9.8.7-" + {
                    ("Darwin", "arm64"): "darwin-arm64",
                    ("Linux", "x86_64"): "linux-x64",
                    ("Linux", "aarch64"): "linux-aarch64",
                }[(platform, architecture)] + ".zip",
            }
            master, slave = pty.openpty()
            process = subprocess.Popen(
                ["bash", str(installer)], cwd=root, env=environment,
                stdin=slave, stdout=slave, stderr=slave, preexec_fn=terminal,
            )
            os.close(slave)
            output = bytearray()
            try:
                while True:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        break
                    if not chunk:
                        break
                    output.extend(chunk)
            finally:
                os.close(master)
            code = process.wait(timeout=10)
            assert (code == 0) == success, (mode, output.decode())
            assert (b"SESSION_STARTED" in output) == success, (mode, output.decode())
            if mode == "bad-checksum":
                assert b"checksum does not match" in output, output.decode()
            if mode == "missing":
                assert b"Refusing to install an older release" in output, output.decode()
            print(f"installer {mode} {platform}/{architecture}: passed")


if __name__ == "__main__":
    main()
