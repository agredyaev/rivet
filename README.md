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

Run one command from the workspace Rivet should access. Rivet displays built-in commands to select and lets you add a custom command with its executable and permitted first arguments. It downloads the latest verified release, installs it, asks for the tunnel ID and runtime API key, then starts a tunnel session:

```sh
curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/install.sh | bash -s -- --root "$PWD"
```

Windows PowerShell:

```powershell
& ([scriptblock]::Create((Invoke-RestMethod 'https://raw.githubusercontent.com/agredyaev/rivet/main/install.ps1'))) -Root (Get-Location).Path
```

The platform installers download the latest verified release and forward the workspace to Rivet. Rivet offers `uv`, `mkdir`, `rg`, and `make` with any arguments; `git` is limited to `status`, `diff`, `log`, `show`, `add`, `commit`, `rev-parse`, and `ls-files`; `cargo` is limited to `check`, `test`, `fmt`, `clippy`, and `metadata`. Enter multiple menu numbers separated by commas to select multiple built-ins. Select **Add a custom command** to add one or more commands, each with chosen first arguments or any arguments. Review the displayed scope before continuing. The installers detect the platform, verify the release ZIP with SHA-256, and install under `~/.local/share/rivet/<release>-<checksum>` or `%LOCALAPPDATA%\Rivet\<release>-<checksum>`. The scope applies only to the current session.

The installer downloads the latest stable `tunnel-client` and bundled `cloudflared` from the official OpenAI GitHub release and verifies its SHA-256 checksum. Create the tunnel and runtime API key in OpenAI Platform using the links shown by the launcher. The runtime key is requested each time, entered with hidden input, and not saved. The temporary tunnel profile is removed when the session ends.

Manual release packages are available from [Releases](https://github.com/agredyaev/rivet/releases/latest):

| OS | Architecture | Rust target | Release asset |
| --- | --- | --- | --- |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `rivet-linux-x64.zip` |
| Linux | aarch64 | `aarch64-unknown-linux-gnu` | `rivet-linux-aarch64.zip` |
| macOS | arm64 | `aarch64-apple-darwin` | `rivet-darwin-arm64.zip` |
| Windows | x64 | `x86_64-pc-windows-gnu` | `rivet-windows-x64.zip` |

Each release package includes Rivet, a starter config, and the platform launchers. Manual download users can extract it and run the launcher directly:

```sh
./start-rivet.sh
```

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\start-rivet.ps1
```

The direct launcher asks Rivet to collect the workspace and command scope for one tunnel session. To set the root in advance, pass `--root`; Rivet still presents the command selector.

```sh
./start-rivet.sh --root /path/to/project \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

It downloads the latest stable `tunnel-client` and bundled `cloudflared` from the official OpenAI GitHub release, verifies the SHA-256 checksum, asks for the tunnel ID and runtime API key with hidden input, then starts the tunnel with this scope. Create the tunnel and runtime key in OpenAI Platform using the links shown by the launcher. Keep the terminal open while you use Rivet through ChatGPT. The runtime key is requested each time and is not saved. The tunnel profile is temporary and removed when the session ends.

The package launcher is for a remote tunnel. If your MCP host starts Rivet directly over stdio, use `bin/rivet` from the package (or `target/release/rivet` from a source build) and supply an existing workspace root:

```sh
./bin/rivet serve --config ./rivet.toml --root /path/to/project \
  --allow-command git=git --allow-subcommand git=status --allow-subcommand git=diff
```

On Windows use `bin\\rivet.exe` and the equivalent flags `--root`, `--allow-command`, and `--allow-subcommand`. Rivet reads `./rivet.toml` by default; pass `--config PATH` for another config. Repeat `--root PATH` for multiple roots and command flags to extend the session scope.

Follow [MCP setup](docs/mcp-setup.md) to connect Rivet to ChatGPT through Secure MCP Tunnel. The TOML file stores resource limits and environment policy; workspace roots and allowed programs are passed to Rivet as startup arguments for each session.

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

CI runs formatting, Clippy, tests, and smoke checks on macOS, Linux, and Windows. Linux x64/aarch64 and Windows x64 GNU builds use `cargo-zigbuild`; GitHub releases contain ZIP archives named `rivet-linux-x64.zip`, `rivet-linux-aarch64.zip`, `rivet-darwin-arm64.zip`, and `rivet-windows-x64.zip`.
