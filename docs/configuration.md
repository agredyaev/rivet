# Startup options and limits

All Rivet modes use built-in resource limits and environment policies. No startup configuration file is required or loaded. Workspace roots and command permissions are selected at startup.

Run `rivet --version` to print the binary version and `rivet --help` for the command list. Add `--help` or `-h` after `session`, `serve`, `doctor`, or `commands` to print that command's options.

## Session scope

Native `rivet session` uses the current directory as its only workspace root. Child commands run inside the OS sandbox, with network access disabled unless `--network` is supplied. See the [security model](security.md).

For MCP stdio, pass each permitted filesystem root on the command line. Roots must exist; relative paths resolve from the launch directory:

```sh
rivet serve --root /path/to/project
```

Repeat `--root PATH` to allow more than one stdio root. Supply the same roots and command permissions to `doctor` and `commands`. Rivet confines its file operations and command working directories to these roots; stdio mode does not apply the native session's OS sandbox.

## Resource limits

These fixed limits apply to both native sessions and stdio:

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

Completed retained entries do not consume live-process capacity. `run_command` submits a managed process and returns immediately; its missing `timeout_ms` uses the default command timeout. `start_process` keeps stdin available and uses the maximum command timeout when no timeout is supplied. Request-specific timeouts and limits must be positive and cannot exceed their built-in bounds.

## Environment

Child processes start with an empty environment. Stdio mode inherits only `PATH`, `HOME`, `TMPDIR`, and `LANG` from Rivet. Native sessions inherit only `PATH` and `LANG`; the sandbox supplies isolated home and temporary directories and the required toolchain paths.

In either mode, MCP requests may override only `RUST_LOG`. Environment values cannot contain NUL. Tunnel credentials are not inherited by child commands.

## Allowed programs

Pass permitted programs at server startup. The command name used by MCP requests is the name before `=`. Restricted commands need one or more allowed first arguments:

```sh
rivet serve \
  --root /path/to/project \
  --allow-command git=git \
  --allow-subcommand git=status \
  --allow-subcommand git=diff
```

Repeat `--allow-command NAME=EXECUTABLE` for each program and `--allow-subcommand NAME=VALUE` for each permitted first argument. `EXECUTABLE` is resolved from `PATH`; a value containing a path separator is treated as a path. `--allow-any-args NAME` explicitly permits any arguments for that command. Stdio mode checks argument paths; native mode relies on the OS sandbox for filesystem access. Allowing a shell or interpreter with unrestricted arguments permits execution of code supplied in those arguments.

Native sessions accept the same command flags and otherwise offer interactive command selection. Rivet passes arguments directly to the executable; it does not invoke a shell or interactive terminal. Setup validates command executables and allows replacing a missing executable before continuing. The final session summary shows resolved commands, allowed first arguments, and child network access after validation. Ctrl+C or EOF cancels setup; hidden credential prompts restore terminal echo before returning. Restart with different flags to change the scope.

## Validate and diagnose

```sh
rivet doctor --root /path/to/project
rivet commands --root /path/to/project \
  --allow-command git=git --allow-subcommand git=status
```

`doctor` validates roots and command permissions, resolves allowed executables, and checks root access and stdio descriptors. `commands` prints the resolved command paths and argument policy. Native sessions validate their workspace, selected commands, and sandbox before connecting to the tunnel.
