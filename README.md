# Rivet

[![CI](https://img.shields.io/github/actions/workflow/status/agredyaev/rivet/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/agredyaev/rivet?style=flat-square)](https://github.com/agredyaev/rivet/releases/latest)

Rivet is a local MCP runtime with a native OpenAI Secure MCP Tunnel transport.

It provides:
- command execution limited to commands allowed for the current session;
- native-session file operations limited to the current workspace;
- long-running child processes.

Rivet does not listen on a network port.

## Quick start

Run the installer from the workspace Rivet should access.

macOS / Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.sh | bash
```

Windows is supported through WSL2. Install and run the Linux build from a WSL2 project directory; native Windows binaries are not shipped. Linux/WSL2 requires the `bubblewrap` package; Ubuntu 24.04 also requires the `bwrap-userns-restrict` AppArmor profile from `apparmor-profiles`. macOS uses the built-in Seatbelt sandbox.

The installer downloads the latest release and starts `rivet session`.

During session setup, Rivet:
1. asks which commands to allow;
2. asks for the OpenAI tunnel ID and runtime API key with terminal echo disabled;
3. starts the native outbound HTTPS tunnel for the current directory and selected command scope.

Create the tunnel in [OpenAI Platform tunnel settings](https://platform.openai.com/settings/organization/tunnels) and create the runtime API key in [OpenAI Platform API keys](https://platform.openai.com/api-keys). Rivet does not save either value. The native tunnel keeps session scope only in memory.

Built-in command presets:
- `uv`, `mkdir`, `rg`, `make`: any arguments;
- `git`: `status`, `diff`, `log`, `show`, `add`, `commit`, `rev-parse`, `ls-files`;
- `cargo`: `check`, `test`, `fmt`, `clippy`, `metadata`, `build`.

Custom commands can be added during setup with either unrestricted arguments or an allowed set of first arguments.

## Manual run

Download a package from [GitHub Releases](https://github.com/agredyaev/rivet/releases/latest), extract it, and run:

macOS / Linux:

```sh
cd /path/to/project
./bin/rivet session
```

The current directory is always the native session workspace. Child network access is disabled by default; add `--network` when builds need to download dependencies. Command scope can still be supplied non-interactively:

```sh
./bin/rivet session \
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

`rivet session` uses the current directory as its only workspace root and requires no configuration file. The legacy stdio commands (`serve`, `doctor`, `commands`, `config-check`) continue to accept `--config` and `--root`.

See [MCP setup](docs/mcp-setup.md) for Secure MCP Tunnel configuration.

## Documentation

- [Configuration](docs/configuration.md)
- [MCP tools](docs/tools.md)
- [MCP setup](docs/mcp-setup.md)
- [Security model](docs/security.md)
- [Development and CI](docs/development.md)
- [Performance measurement procedure](docs/measurements.md)

## Build from source

Requires Rust with Edition 2024 support.

```sh
git clone https://github.com/agredyaev/rivet.git
cd rivet
cargo build --locked --release
```

Run `rivet --help`, `rivet serve --help`, or `rivet session --help` for CLI details.
