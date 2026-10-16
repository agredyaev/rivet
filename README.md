# Rivet

[![CI](https://img.shields.io/github/actions/workflow/status/agredyaev/rivet/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/agredyaev/rivet?style=flat-square)](https://github.com/agredyaev/rivet/releases/latest)

Rivet connects ChatGPT to one local workspace through OpenAI Secure MCP Tunnel over outbound HTTPS. It exposes filesystem tools and explicitly selected executables. Child commands always run inside the native OS sandbox.

Rivet does not listen on a network port.

## Quick start

Run the installer from the workspace Rivet should access:

```sh
curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.sh | bash
```

Windows is supported through WSL2. Linux/WSL2 requires `bubblewrap`; Ubuntu 24.04 also requires the `bwrap-userns-restrict` AppArmor profile from `apparmor-profiles`. macOS uses the built-in Seatbelt sandbox.

The installer verifies the published release, installs Rivet under `~/.local/share/rivet` by default, and starts `rivet session`.

Interactive setup:
1. select built-in or custom executables;
2. choose whether child commands need network access, for example for package or dependency downloads;
3. choose whether child commands need specific parent environment variables such as API keys or tokens;
4. enter the OpenAI tunnel ID and runtime API key with terminal echo disabled;
5. start the outbound HTTPS tunnel for the current workspace.

Environment-variable values are never printed or saved. Rivet captures only variables explicitly selected with `--pass-env`; `PATH` is retained as the runtime executable-search substrate.

Built-in command choices are `uv`, `mkdir`, `rg`, `git`, `cargo`, and `make`. Registering an executable authorizes that executable; Rivet passes its argv directly without interpreting shell syntax.

## Manual run

Download the archive for your platform from [GitHub Releases](https://github.com/agredyaev/rivet/releases/latest), extract it, and start Rivet from the workspace:

```sh
cd /path/to/project
/absolute/path/to/extracted-package/bin/rivet session
```

The current directory is the only workspace root. Child network access is disabled by default.

A non-interactive example:

```sh
/absolute/path/to/extracted-package/bin/rivet session \
  --network \
  --pass-env FRED_API_KEY \
  --allow-command uv=uv \
  --allow-command git=git \
  --allow-command cargo=cargo \
  --allow-command make=make
```

`--allow-command NAME=EXECUTABLE`, `--pass-env NAME`, and `--network` are startup capabilities. MCP requests cannot add executables, environment variables, filesystem roots, or network access.

See [MCP setup](docs/mcp-setup.md) for Secure MCP Tunnel configuration.

## Documentation

- [Startup options and limits](docs/configuration.md)
- [MCP tools](docs/tools.md)
- [MCP setup](docs/mcp-setup.md)
- [Security model](docs/security.md)
- [Development and CI](docs/development.md)
- [Performance measurement procedure](docs/measurements.md)

## Build from source

Use the stable Rust toolchain:

```sh
git clone https://github.com/agredyaev/rivet.git
cd rivet
cargo build --locked --release
```

Run `target/release/rivet --help` or `target/release/rivet session --help` for CLI details.

Rivet is licensed under [MIT](LICENSE).
