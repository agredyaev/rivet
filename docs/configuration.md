# Configuration

Rivet reads TOML from `./rivet.toml` unless `--config PATH` is supplied. Unknown fields are rejected. Start with [`rivet.example.toml`](../rivet.example.toml).

## Filesystem roots

```toml
[filesystem]
allowed_roots = ["/absolute/path/to/project"]
```

Roots must exist and be absolute. At least one root is required. On Windows, TOML literal strings are convenient for paths, for example `allowed_roots = ['C:\Users\you\project']`. `--root PATH` may be repeated; when present, command-line roots replace the configured list for that run. Relative command-line roots resolve from the launch directory.

Rivet confines its file operations and command working directories to these roots. See the [security model](security.md) for the limits of this boundary.

## Resource limits

```toml
[limits]
max_stdout_bytes = 2097152
max_stderr_bytes = 2097152
max_file_read_bytes = 4194304
max_directory_entries = 1000
max_running_processes = 8
default_timeout_ms = 120000
max_timeout_ms = 1800000
```

All values must be positive. `max_file_read_bytes` cannot exceed 64 MiB. `default_timeout_ms` cannot exceed `max_timeout_ms`; the maximum timeout cannot exceed 4,294,967,295 ms. Output and directory limits cap returned or retained data. The maximum timeout applies to both command and process requests.

## Environment

```toml
[environment]
pass = ["PATH", "HOME", "TMPDIR", "LANG"]
allow_override = ["RUST_LOG"]
```

Child processes start with an empty environment. Rivet copies only variables named in `pass` from its own environment. An MCP request may override only names listed in `allow_override`. Variable names cannot be empty or contain `=` or NUL.

## Command registry

Each command needs one argument authorization mode:

```toml
[commands.git]
executable = "git"
allowed_subcommands = ["status", "diff", "log", "show"]

[commands.cargo]
executable = "cargo"
allow_any_args = true
```

`executable` is resolved from `PATH` at startup, or treated as a path if it contains a slash. With `allow_any_args = true`, all arguments are accepted subject to the path checks described in the [security model](security.md). Otherwise, `allowed_subcommands` must be nonempty and the first argument must match one of its entries exactly. Rivet passes arguments directly to the executable; it does not invoke a shell implicitly.

Commands are resolved when Rivet starts. Restart the server after changing the command registry or environment configuration.

## Validate and diagnose

```sh
rivet config-check
rivet doctor
rivet commands
```

`config-check` validates and loads the configuration. `doctor` also checks root access and configured executables. `commands` prints the resolved command paths.
