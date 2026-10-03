# Rivet

[![CI](https://img.shields.io/github/actions/workflow/status/agredyaev/rivet/ci.yml?branch=main&style=for-the-badge&logo=githubactions&logoColor=white&label=CI)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/agredyaev/rivet?style=for-the-badge&logo=github&logoColor=white)](https://github.com/agredyaev/rivet/releases/latest)

![Rust 2024](https://img.shields.io/badge/Rust-2024-18181B?style=for-the-badge&logo=rust&logoColor=white)
![Tokio](https://img.shields.io/badge/Tokio-async-7C3AED?style=for-the-badge&logo=tokio&logoColor=white)
![Zig](https://img.shields.io/badge/Zig-cross--compilation-F7A41D?style=for-the-badge&logo=zig&logoColor=white)
![MCP stdio](https://img.shields.io/badge/MCP-stdio-0EA5E9?style=for-the-badge)

![Windows](https://img.shields.io/badge/Windows-x86__64-0078D4?style=for-the-badge)
![Linux](https://img.shields.io/badge/Linux-x86__64%20%7C%20aarch64-FCC624?style=for-the-badge&logo=linux&logoColor=black)
![macOS](https://img.shields.io/badge/macOS-aarch64-18181B?style=for-the-badge&logo=apple&logoColor=white)

Rivet implements an [MCP](https://modelcontextprotocol.io/) server over stdio. It exposes configured commands, file operations within configured roots, and long-running child processes. It does not listen on a network port.

## Install and run

Download the matching binary from [Releases](https://github.com/agredyaev/rivet/releases/latest) and download [`rivet.example.toml`](rivet.example.toml):

| OS | Architecture | Rust target | Release asset |
| --- | --- | --- | --- |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` | `rivet-x86_64-unknown-linux-gnu` |
| Linux | aarch64 | `aarch64-unknown-linux-gnu` | `rivet-aarch64-unknown-linux-gnu` |
| macOS | arm64 (aarch64) | `aarch64-apple-darwin` | `rivet-aarch64-apple-darwin` |
| Windows | x86_64 | `x86_64-pc-windows-gnu` | `rivet-x86_64-pc-windows-gnu.exe` |

On macOS or Linux, rename the downloaded file to `rivet`. On Windows, keep the `.exe` extension.

Copy the example file to `rivet.toml`. Set `filesystem.allowed_roots` to an existing absolute directory. The example uses `[]`, which Rivet rejects until you add a root.

```sh
cp rivet.example.toml rivet.toml
```

Edit `rivet.toml`, then run these commands from its directory:

```sh
chmod +x ./rivet
./rivet config-check
./rivet doctor
./rivet serve
```

On Windows PowerShell, copy the configuration, edit `rivet.toml`, then run:

```powershell
Copy-Item rivet.example.toml rivet.toml
notepad rivet.toml
.\rivet.exe config-check
.\rivet.exe doctor
.\rivet.exe serve
```

If you built from source, use `target/release/rivet` (or `target\release\rivet.exe` on Windows). Rivet reads `./rivet.toml` by default. Pass `--config PATH` to select another file. Repeat `--root PATH` to replace the configured roots for a session:

```sh
./rivet serve --config /path/to/rivet.toml --root /path/to/project
```

Follow [MCP client setup](docs/mcp-setup.md) to register Rivet in Codex, ChatGPT Desktop, or ChatGPT Web. The guide also explains the tunnel required by ChatGPT Web. Keep `rivet.toml` private because it contains local paths and the command allowlist.

## Documentation

- [Configuration reference](docs/configuration.md)
- [MCP tools reference](docs/tools.md)
- [MCP client setup](docs/mcp-setup.md)
- [Security model](docs/security.md)
- [Build, CI, and profiling](docs/development.md)
- [Release measurements](docs/measurements.md)

## Build from source

Requires Rust with Edition 2024 support:

```sh
git clone https://github.com/agredyaev/rivet.git
cd rivet
cargo build --locked --release
```

CI runs formatting, Clippy, tests, and smoke checks on macOS, Linux, and Windows. Linux x86_64/aarch64 and Windows x86_64 GNU builds use `cargo-zigbuild`; GitHub releases contain binaries for those targets plus macOS arm64.
