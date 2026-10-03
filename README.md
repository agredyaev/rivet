# Rivet

[![CI](https://img.shields.io/github/actions/workflow/status/agredyaev/rivet/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/agredyaev/rivet?style=flat-square)](https://github.com/agredyaev/rivet/releases/latest)

Rivet is an [MCP](https://modelcontextprotocol.io/) server over stdio.

It provides:
- command execution limited to commands allowed for the current session;
- file operations limited to configured roots;
- long-running child processes.

Rivet does not listen on a network port.

## Quick start

Run the installer from the workspace Rivet should access.

macOS / Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.sh | bash -s -- --root "$PWD"
```

Windows PowerShell:

```powershell
& ([scriptblock]::Create((Invoke-RestMethod 'https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.ps1'))) -Root (Get-Location).Path
```

The installer downloads the latest release and starts `rivet session`.

During session setup, Rivet:
1. asks which commands to allow;
2. asks for the OpenAI tunnel ID and runtime API key;
3. downloads and verifies the required tunnel client;
4. starts the tunnel for the selected workspace and command scope.

The runtime API key is entered with hidden input and is not saved. The temporary tunnel profile is removed when the session ends.

Built-in command presets:
- `uv`, `mkdir`, `rg`, `make`: any arguments;
- `git`: `status`, `diff`, `log`, `show`, `add`, `commit`, `rev-parse`, `ls-files`;
- `cargo`: `check`, `test`, `fmt`, `clippy`, `metadata`.

Custom commands can be added during setup with either unrestricted arguments or an allowed set of first arguments.

## Manual run

Download a package from [GitHub Releases](https://github.com/agredyaev/rivet/releases/latest), extract it, and run:

macOS / Linux:

```sh
./bin/rivet session
```

Windows:

```powershell
powershell.exe -NoProfile -Command "& .\bin\rivet.exe session"
```

Set the workspace root and command scope explicitly when needed:

```sh
./bin/rivet session --root /path/to/project \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

## Direct stdio server

If the MCP host starts Rivet directly instead of using `rivet session`:

```sh
./bin/rivet serve --config ./rivet.toml --root /path/to/project \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

Rivet reads `./rivet.toml` by default. Use `--config PATH` for another file. Repeat `--root` and command flags to extend the session scope.

See [MCP setup](docs/mcp-setup.md) for Secure MCP Tunnel configuration.

## Documentation

- [Configuration](docs/configuration.md)
- [MCP tools](docs/tools.md)
- [MCP setup](docs/mcp-setup.md)
- [Security model](docs/security.md)
- [Development and CI](docs/development.md)
- [Release measurements](docs/measurements.md)

## Build from source

Requires Rust with Edition 2024 support.

```sh
git clone https://github.com/agredyaev/rivet.git
cd rivet
cargo build --locked --release
```

Run `rivet --help`, `rivet serve --help`, or `rivet session --help` for CLI details.
