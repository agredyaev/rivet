# Configuration

Rivet reads TOML from `./rivet.toml` unless `--config PATH` is supplied. Unknown fields are rejected. Start with [`rivet.example.toml`](../rivet.example.toml).

Run `rivet --help` for the command list. Add `--help` or `-h` after `serve`, `doctor`, `commands`, or `config-check` to print that command's options. `rivet session --help` prints the interactive session options without reading the config file.

## Session scope

Pass each permitted filesystem root on the command line. Roots must exist; relative paths resolve from the launch directory.

```sh
rivet serve --config ./rivet.toml --root /path/to/project
```

Repeat `--root PATH` to allow more than one root. The same roots must be supplied to `config-check`, `doctor`, and `commands`. Rivet confines its file operations and command working directories to these roots. See the [security model](security.md) for the limits of this boundary.

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

## Allowed programs

Pass permitted programs at server startup. The command name used by MCP requests is the name before `=`. Restricted commands need one or more allowed first arguments:

```sh
rivet serve --config ./rivet.toml \
  --root /path/to/project \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

Repeat `--allow-command NAME=EXECUTABLE` for each program and `--allow-subcommand NAME=VALUE` for each permitted first argument. `EXECUTABLE` is resolved from `PATH`; a value containing a path separator is treated as a path. `--allow-any-args NAME` explicitly permits any arguments for that command, subject to path checks. Avoid it for shells and interpreters.

Rivet passes arguments directly to the executable; it does not invoke a shell or interactive terminal. The launcher passes this scope to the server for the current tunnel session. Restart with different flags to change it.

## Validate and diagnose

```sh
rivet config-check --config ./rivet.toml --root /path/to/project
rivet doctor --config ./rivet.toml --root /path/to/project
rivet commands --config ./rivet.toml --root /path/to/project \
  --allow-command git=git --allow-subcommand git=status
```

`config-check` validates and loads the configuration. `doctor` also checks root access and allowed executables. `commands` prints the resolved command paths and argument policy.
