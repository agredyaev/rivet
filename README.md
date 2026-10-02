# Rivet

[![CI](https://img.shields.io/github/actions/workflow/status/agredyaev/rivet/ci.yml?branch=main&style=for-the-badge&logo=githubactions&logoColor=white&label=CI)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/agredyaev/rivet?style=for-the-badge&logo=github&logoColor=white)](https://github.com/agredyaev/rivet/releases/latest)

![Rust 2024](https://img.shields.io/badge/Rust-2024-18181B?style=for-the-badge&logo=rust&logoColor=white)
![Tokio](https://img.shields.io/badge/Tokio-async-7C3AED?style=for-the-badge&logo=tokio&logoColor=white)
![Zig](https://img.shields.io/badge/Zig-cross--compilation-F7A41D?style=for-the-badge&logo=zig&logoColor=white)
![MCP stdio](https://img.shields.io/badge/MCP-stdio-0EA5E9?style=for-the-badge)

![Windows](https://img.shields.io/badge/Windows-x64-0078D4?style=for-the-badge)
![Linux](https://img.shields.io/badge/Linux-x64-FCC624?style=for-the-badge&logo=linux&logoColor=black)
![macOS](https://img.shields.io/badge/macOS-arm64-18181B?style=for-the-badge&logo=apple&logoColor=white)

Rivet is a local [MCP](https://modelcontextprotocol.io/) server. It lets an MCP client access configured commands, files, and long-running processes over standard input and output. Rivet does not open a network port; connect remote clients through a separate tunnel service.

## Install and run

Download the binary for your platform from [Releases](https://github.com/agredyaev/rivet/releases/latest) and get the [example configuration](rivet.example.toml). The current release includes macOS arm64, Linux x64, and Windows x64 builds. On macOS or Linux, rename the binary to `rivet` and make it executable. On Windows, keep the name `rivet.exe`.

Copy `rivet.example.toml` to `rivet.toml`. Set `filesystem.allowed_roots` to an existing absolute directory and configure the commands Rivet may run. Validate the file, check the environment, then start the server:

```sh
cp rivet.example.toml rivet.toml
# Edit rivet.toml for your machine
chmod +x ./rivet
./rivet config-check
./rivet doctor
./rivet serve
```

On Windows PowerShell, copy the configuration with `Copy-Item rivet.example.toml rivet.toml`, edit it, then run:

```powershell
.\rivet.exe config-check
.\rivet.exe doctor
.\rivet.exe serve
```

If you built from source, use `target/release/rivet` (or `target\release\rivet.exe` on Windows). Rivet reads `./rivet.toml` by default. Pass `--config PATH` to select another file. Repeat `--root PATH` to replace the configured roots for a session:

```sh
rivet serve --config /path/to/rivet.toml --root /path/to/project
```

Configure your MCP client to launch `rivet serve` as a stdio process. Keep `rivet.toml` private: it contains local paths and command permissions.

## Documentation

- [Configuration reference](docs/configuration.md)
- [MCP tools reference](docs/tools.md)
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

CI runs formatting, Clippy, tests, and smoke checks on macOS, Linux, and Windows. The Linux and Windows x64 release targets are cross-compiled with `cargo-zigbuild` and smoke-tested on their target systems.
