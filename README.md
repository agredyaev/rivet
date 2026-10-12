# Rivet

[![CI](https://img.shields.io/github/actions/workflow/status/agredyaev/rivet/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/agredyaev/rivet/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/agredyaev/rivet?style=flat-square)](https://github.com/agredyaev/rivet/releases/latest)

Rivet is an MCP server that connects to ChatGPT through OpenAI Secure MCP Tunnel over outbound HTTPS.

This documentation describes the current source. For changes that have not been released yet, use the source build below; the installer downloads a published release.

It runs commands selected at startup, reads and writes workspace files, and tracks up to 8 running child processes and 32 retained process entries.

Rivet does not listen on a network port.

## Quick start

Run the installer in an interactive terminal from the workspace Rivet should access.

macOS arm64 / Linux x64 or arm64:

```sh
curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.sh | bash
```

Windows is supported through WSL2. Install and run the Linux build from a WSL2 project directory; native Windows binaries are not shipped. Linux/WSL2 requires the `bubblewrap` package; Ubuntu 24.04 also requires the `bwrap-userns-restrict` AppArmor profile from `apparmor-profiles`. macOS uses the built-in Seatbelt sandbox.

The installer reads the release version recorded on `main`, verifies the archive checksum, installs under `~/.local/share/rivet` by default, and starts `rivet session`. It refuses to fall back to an older release when the recorded version is unavailable. It does not add `rivet` to `PATH`.

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

Download the archive for your platform from [GitHub Releases](https://github.com/agredyaev/rivet/releases/latest) and extract it. New archive names include the version, for example `rivet-v0.3.0-linux-x64.zip`. The installer also accepts the previous archive name within the same release tag.

Start the session from the workspace directory:

macOS arm64 / Linux x64 or arm64:

```sh
cd /path/to/project
/absolute/path/to/extracted-package/bin/rivet session
```

The current directory is always the native session workspace. Child network access is disabled by default; add `--network` when builds need to download dependencies. Command scope can still be supplied non-interactively:

```sh
/absolute/path/to/extracted-package/bin/rivet session \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

## Direct stdio server

If the MCP host starts Rivet directly instead of using `rivet session`:

```sh
/absolute/path/to/extracted-package/bin/rivet serve --root /path/to/project \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

`rivet session` uses the current directory as its only workspace root. All modes use built-in resource limits and environment policies. For stdio mode, pass each workspace root with `--root`; repeat root and command flags to extend the scope.

See [MCP setup](docs/mcp-setup.md) for Secure MCP Tunnel configuration.

## Documentation

- [Startup options and limits](docs/configuration.md)
- [MCP tools](docs/tools.md)
- [MCP setup](docs/mcp-setup.md)
- [Security model](docs/security.md)
- [Development and CI](docs/development.md)
- [Performance measurement procedure](docs/measurements.md)

## Build from source

Use the stable Rust toolchain. CI installs stable Rust.

```sh
git clone https://github.com/agredyaev/rivet.git
cd rivet
cargo build --locked --release
```

From the checkout, run `target/release/rivet --help`, `target/release/rivet serve --help`, or `target/release/rivet session --help` for CLI details. Other examples use `rivet` as shorthand for the binary's absolute path or a binary already on `PATH`.

Rivet is licensed under [MIT](LICENSE).
