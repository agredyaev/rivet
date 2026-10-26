# Startup options and limits

Rivet has one runtime command: `rivet session`. No startup configuration file is required or loaded. The current directory is the only workspace root.

Run `rivet --version` to print the binary version, `rivet --help` for the command list, and `rivet session --help` for session options.

## Session capabilities

Child commands always run inside the native OS sandbox.

The operator may grant four capabilities at startup:

```sh
rivet session \
  --allow-command git=git \
  --allow-command-path "$HOME/.rustup" \
  --pass-env FRED_API_KEY \
  --network
```

- `--allow-command NAME=EXECUTABLE` registers one executable. Repeat it for additional executables. The executable is resolved at startup.
- `--allow-command-path PATH` grants sandboxed child commands read/write access to one existing absolute host path. Repeat it for additional paths. Paths are canonicalized, sorted, and deduplicated at startup. They do not become MCP filesystem roots and cannot be used as command `cwd` unless they are also inside the workspace. On macOS, Unix-domain sockets beneath the granted path require `--network`; Rivet emits a path-scoped `network-outbound` rule with a `remote unix-socket` filter only when both capabilities are present.
- `--pass-env NAME` captures one existing parent environment variable for child commands. Repeat it for additional variables.
- `--network` enables child network access. Network access is denied by default.

Without `--allow-command` flags, Rivet offers interactive command selection and then asks whether child commands need network access and specific environment variables.

Registering an executable authorizes arbitrary argv for that executable. Rivet passes argv directly to the executable and never inserts a shell. Shell metacharacters therefore remain ordinary argv unless the operator explicitly registers a shell or interpreter and that executable interprets them itself.

## Environment

Child processes start with an empty environment.

Rivet then supplies:

- the startup `PATH` as runtime substrate so registered tools can locate their own child executables;
- Rivet-owned sandbox variables such as isolated `HOME`, `TMPDIR`, and `CARGO_HOME`, plus platform/toolchain variables required by the native sandbox;
- only user/application variables selected with `--pass-env NAME`.

`--pass-env` is fail-closed:

- the name must be a valid environment-variable name;
- duplicate names are rejected;
- the variable must exist in the parent environment when the session starts;
- names owned by Rivet such as `PATH`, `HOME`, `TMPDIR`, `CARGO_HOME`, `RUSTUP_HOME`, `DEVELOPER_DIR`, and `SDKROOT` are reserved.

Values are captured once at startup, kept only in memory, and are not printed in the session summary or exposed through MCP metadata.

MCP command requests cannot provide environment overrides.

## Resource limits

| Resource | Limit |
| --- | ---: |
| Captured stdout per process | 2 MiB |
| Captured stderr per process | 2 MiB |
| File reads, write content per call, and replacement results | 4 MiB |
| Directory entries per listing | 1000 |
| Concurrent managed processes | 8 |
| Retained process entries, including running processes | 32 |
| Default command timeout | 120,000 ms (2 minutes) |
| Maximum command timeout | 1,800,000 ms (30 minutes) |

Completed retained entries do not consume live-process capacity. `run_command` submits a managed process and returns immediately. `start_process` keeps stdin available. Request-specific timeouts must be positive and cannot exceed the built-in maximum.

## Startup validation

Before connecting to the tunnel, Rivet:

1. canonicalizes and opens the current workspace;
2. verifies required workspace access;
3. resolves every registered executable;
4. validates and captures each `--pass-env` variable;
5. verifies that the mandatory native sandbox backend is available.

If any required boundary cannot be established, startup fails closed.
