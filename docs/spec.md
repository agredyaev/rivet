# Rivet v0.1 — Lightweight Local MCP Command Gateway

## 1. Purpose

Rivet is a minimal Rust MCP server that gives an AI client controlled access to a local development machine.

Primary use case:

```text
ChatGPT / MCP Client
        │
        │ MCP over stdio
        ▼
      Rivet
        │
        ├── filesystem
        ├── commands
        └── long-running processes
        │
        ▼
        Local macOS, Linux, or Windows machine
```

Rivet must remain generic. It must not contain Cargo-, UV-, Git-, Pytest-, or project-specific business logic.

The implementation must prefer the smallest explicit solution and avoid speculative infrastructure or abstraction.

---

## 2. Scope

### v0.1 includes

- MCP stdio transport.
- Config-driven command registry.
- Generic `run_command`.
- Long-running process management.
- File reading/writing.
- Directory listing.
- Exact text replacement for safe source edits.
- Allowed-root enforcement.
- Command/subcommand authorization.
- Output and runtime limits.
- Process-tree termination, including child processes.

### v0.1 explicitly excludes

- HTTP server.
- TLS.
- OAuth.
- networking code.
- GUI.
- database.
- plugins.
- command-specific Rust modules.
- project auto-detection.
- task runner.
- Git abstraction.
- Cargo abstraction.
- UV abstraction.
- patch framework.
- dependency injection.
- persistent runtime state.

Connectivity to ChatGPT is handled externally by the OpenAI tunnel client.

---

## 3. Design Targets

Release build targets:

```text
single binary
idle CPU: effectively 0
idle RAM target: <= 15 MB
binary target: <= 5 MB stripped
startup target: <= 50 ms
no background polling
no network listener
```

These are engineering targets, not reasons to introduce optimization complexity before measurement.

---

## 4. MCP Tools

Rivet v0.1 exposes only the following tools.

### Command tools

```text
list_commands
run_command
```

### Process tools

```text
start_process
read_process_output
send_process_input
stop_process
```

### Filesystem tools

```text
list_roots
list_directory
read_file
write_file
replace_text
```

Total: **11 MCP tools**.

No tool exists specifically for:

```text
cargo
uv
git
pytest
ruff
make
rg
```

Those are command-registry entries.

---

## 5. Command Registry

Commands are defined entirely in `rivet.toml`.

Example:

```toml
[commands.cargo]
executable = "cargo"
allow_any_args = true

[commands.uv]
executable = "uv"
allow_any_args = true

[commands.git]
executable = "git"
allowed_subcommands = [
    "status",
    "diff",
    "log",
    "show",
    "branch",
    "switch",
    "checkout",
    "add",
    "commit",
    "push",
    "pull",
    "fetch"
]

[commands.rg]
executable = "rg"
allow_any_args = true

[commands.make]
executable = "make"
allow_any_args = true
```

Adding another executable requires only a configuration change:

```toml
[commands.just]
executable = "just"
allow_any_args = true
```

No recompilation is required.

---

## 6. Command Resolution

At startup Rivet resolves every configured executable once.

Example internal representation:

```text
cargo → /Users/user/.cargo/bin/cargo
uv    → /opt/homebrew/bin/uv
git   → /usr/bin/git
rg    → /opt/homebrew/bin/rg
```

Failure to resolve an enabled command makes configuration validation fail.

Runtime execution uses the resolved absolute executable path.

No repeated PATH lookup is required.

---

## 7. `list_commands`

Returns the current command registry.

Example:

```json
{
  "commands": [
    {
      "name": "cargo",
      "allow_any_args": true
    },
    {
      "name": "git",
      "allowed_subcommands": [
        "status",
        "diff",
        "log",
        "commit",
        "push"
      ]
    }
  ]
}
```

The AI can therefore discover available local commands without hardcoded knowledge in Rivet.

`list_roots` returns the effective canonical allowed roots, including any
launch-time `--root` overrides. The AI can use a returned path for filesystem
tools or as `cwd` for command tools.

---

## 8. `run_command`

Request:

```rust
struct RunCommandRequest {
    command: String,
    args: Vec<String>,
    cwd: String,
    timeout_ms: Option<u64>,
    env: BTreeMap<String, String>,
}
```

Example:

```json
{
  "command": "uv",
  "args": [
    "run",
    "pytest",
    "tests/test_mm09.py",
    "-q"
  ],
  "cwd": "/Users/user/dev/rigor-signals",
  "timeout_ms": 120000
}
```

Response:

```rust
struct CommandResult {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    timed_out: bool,
    truncated: bool,
}
```

---

## 9. Argument Passing and Shells

Rivet must execute:

```rust
Command::new(executable)
    .args(args)
```

Rivet passes arguments directly to the configured executable. It does not
implicitly invoke a shell to interpret them. For example, these arguments:

```text
sh -c ...
bash -c ...
zsh -c ...
```

are passed as ordinary arguments. Shell syntax such as:

```text
&&
||
;
|
>
$()
`...`
```

has no special meaning to Rivet. The configured executable may interpret its
arguments; for example, registering `sh` with unrestricted arguments lets a
client ask that shell to run a script. Do so only when that access is intended.

---

## 10. Argument Authorization

Two authorization modes exist in v0.1.

### Mode A — arbitrary arguments

```toml
[commands.cargo]
allow_any_args = true
```

Examples allowed:

```text
cargo test --workspace
cargo clippy --all-targets
cargo check -p foo
```

### Mode B — allowed subcommands

```toml
[commands.git]
allowed_subcommands = [
    "status",
    "diff",
    "log",
    "commit",
    "push"
]
```

Rivet validates:

```text
args[0]
```

Example:

```text
git commit -m "change"
    ↓
commit ∈ allowed_subcommands
    ↓
ALLOW
```

Whereas:

```text
git clean -fd
    ↓
clean ∉ allowed_subcommands
    ↓
COMMAND_DENIED
```

No flag-level authorization DSL exists in v0.1.

---

## 11. Filesystem Security

Configuration:

```toml
[filesystem]
allowed_roots = [
    "/Users/user/dev",
    "/Users/user/work"
]
```

Every filesystem path and command `cwd` must be absolute and resolve inside one
of the effective roots. Rivet opens file paths and working directories relative to an
open root directory capability. In-root relative symlinks are allowed;
symlink escapes and final symlinks for writes are rejected.

The root rule governs Rivet's file operations and command working directories.
Rivet also rejects direct command arguments and `key=value` argument values
whose paths resolve outside the roots, including paths through symlinks.
This is a conservative argument check, not an OS sandbox: registered executables
retain their ordinary permissions and can interpret embedded paths or read
paths from configuration and the environment.

Reject:

```text
../ escape
absolute paths outside allowed roots
symlink escape
non-canonical parent escape
```

Required check conceptually:

```text
requested absolute path
    ↓
map to configured root and open relative to its directory capability
    ├── stays within root → continue
    └── escapes root → PATH_DENIED
```

---

## 12. Filesystem Tools

### `list_directory`

Inputs:

```text
path
depth
max_entries
```

Must enforce a bounded result.

### `read_file`

Inputs:

```text
path
offset
limit
```

Must support partial reads.

Offsets and limits are bytes. The returned UTF-8 text includes `next_offset`
and `eof`. An offset inside a character or invalid UTF-8 is rejected.

Must not load arbitrarily large files into memory.

### `write_file`

Inputs:

```text
path
content
mode = create | overwrite | append
expected_sha256 = optional
```

When `expected_sha256` is provided, modification fails if the current file differs.

This provides simple optimistic concurrency protection.

### `replace_text`

Inputs:

```text
path
old
new
expected_occurrences
expected_sha256 = optional
```

Example:

```text
expected_occurrences = 1
```

If the actual match count is different, the operation fails without modifying the file.

This replaces the need for a complex patch engine in v0.1.

---

## 13. Long-Running Processes

`start_process` accepts the same command validation model as `run_command`.
It also accepts optional `timeout_ms`, defaulting to `max_timeout_ms`;
`run_command` defaults to `default_timeout_ms`. Zero and values above the
configured maximum are rejected.

Request:

```text
command
args
cwd
env
```

Response:

```json
{
  "process_id": 42
}
```

Rivet maintains only in-memory process state.

No database or persistent process registry.

### `read_process_output`

Inputs:

```text
process_id
stdout_offset
stderr_offset
limit
```

Returns new or requested bounded output.

### `send_process_input`

Writes bytes/text to the process stdin.

### `stop_process`

Stops the process group on macOS and Linux, and the process tree on Windows. It
must stop descendants as well as the immediate child process.

This prevents orphaned compiler/test subprocesses.

---

## 14. Process Storage

Use one owned process table:

```rust
struct ProcessTable {
    entries: HashMap<ProcessId, ProcessEntry>,
}
```

`ProcessId` is a typed integer ID:

```rust
#[repr(transparent)]
struct ProcessId(u64);
```

Do not build an object graph or multiple process registries.

The runtime should maintain one canonical representation rather than duplicated state.

---

## 15. Output Handling

Configuration:

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

Output must be bounded.

When exceeded:

```json
{
  "truncated": true
}
```

Do not retain unlimited process output in memory.

Use bounded buffers.

---

## 16. Environment Variables

Do not expose the parent environment to the MCP client.

Environment inherited by commands is controlled by configuration.

Example:

```toml
[environment]
pass = [
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "CARGO_HOME",
    "RUSTUP_HOME"
]

allow_override = [
    "RUST_LOG",
    "CARGO_TERM_COLOR"
]
```

Request-supplied environment variables not present in `allow_override` are rejected.

Rivet never returns the complete process environment.

---

## 17. Configuration

Example complete minimal configuration:

```toml
[filesystem]
allowed_roots = [
    "/Users/user/dev"
]

[limits]
max_stdout_bytes = 2097152
max_stderr_bytes = 2097152
max_file_read_bytes = 4194304
max_directory_entries = 1000
max_running_processes = 8
default_timeout_ms = 120000
max_timeout_ms = 1800000

[environment]
pass = [
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "CARGO_HOME",
    "RUSTUP_HOME"
]

allow_override = [
    "RUST_LOG"
]

[commands.cargo]
executable = "cargo"
allow_any_args = true

[commands.uv]
executable = "uv"
allow_any_args = true

[commands.git]
executable = "git"
allowed_subcommands = [
    "status",
    "diff",
    "log",
    "show",
    "branch",
    "switch",
    "checkout",
    "add",
    "commit",
    "push",
    "pull",
    "fetch"
]

[commands.rg]
executable = "rg"
allow_any_args = true
```

---

## 18. CLI

Required local CLI:

```text
rivet serve
rivet doctor
rivet commands
rivet config-check
```

### `rivet serve`

Starts MCP over stdin/stdout.

### `rivet doctor`

Checks:

```text
config readable
allowed roots valid
configured executables resolve
permissions valid
limits valid
stdio available
```

### `rivet commands`

Prints the resolved command registry.

### `rivet config-check`

Validates configuration without starting MCP.

Every CLI command accepts `--config PATH` and repeatable `--root PATH`. If any
`--root` is supplied, those roots replace `filesystem.allowed_roots` for that
invocation. A relative `--root` is resolved from the launch working directory;
the effective roots must exist when Rivet starts. A portable config can use
`allowed_roots = []` and supply roots at launch:

```text
rivet serve --config /path/to/rivet.toml --root /path/to/project
```

Adding a project under an existing root needs no restart. Changing the roots
of a running stdio session requires launching a new session.

---

## 19. ChatGPT Connectivity

Rivet contains no networking code.

For ChatGPT:

```text
ChatGPT
   │
OpenAI tunnel
   │
tunnel-client
   │ stdio
Rivet
```

The tunnel client must launch:

```text
rivet serve --config /path/to/rivet.toml
```

For a local MCP client:

```text
MCP Client
    │ stdio
    ▼
  Rivet
```

The same binary is used in both cases.

---

## 20. Rust Project Structure

Keep the source tree small:

```text
rivet/
├── Cargo.toml
├── rivet.example.toml
└── src/
    ├── main.rs
    ├── mcp.rs
    ├── config.rs
    ├── commands.rs
    ├── process.rs
    └── filesystem.rs
```

Do not create:

```text
cargo.rs
uv.rs
git.rs
pytest.rs
ruff.rs
command_plugins/
providers/
backends/
services/
repositories/
```

unless a concrete future requirement justifies them.

---

## 21. Dependency Policy

Prefer standard library where practical.

External dependencies should be limited to actual requirements such as:

```text
serde
serde_json
toml
MCP protocol implementation
small error/logging support if justified
```

Avoid adding a full async runtime unless the MCP implementation or measured process-management requirements justify it.

Do not add an abstraction or framework solely for possible future extensibility.

---

## 22. Release Profile

Recommended:

```toml
[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

Measure resulting size/startup/runtime rather than introducing custom allocators or unsafe optimizations.

---

## 23. Error Contract

Every tool failure returns a stable machine-readable code.

Required codes:

```text
INVALID_REQUEST
CONFIG_ERROR
PATH_DENIED
COMMAND_NOT_FOUND
COMMAND_DENIED
SUBCOMMAND_DENIED
COMMAND_TIMEOUT
PROCESS_NOT_FOUND
PROCESS_LIMIT
OUTPUT_LIMIT
FILE_CHANGED
IO_ERROR
SPAWN_ERROR
```

Example:

```json
{
  "code": "SUBCOMMAND_DENIED",
  "message": "git subcommand is not allowed",
  "details": {
    "command": "git",
    "subcommand": "clean"
  }
}
```

---

## 24. Required Tests

### Command registry

- valid command resolves;
- missing executable fails config validation;
- unknown command rejected;
- `allow_any_args` works;
- allowed subcommand works;
- unknown subcommand rejected.

### Shell safety

Verify that arguments containing:

```text
;
&&
|
>
$()
```

do not execute additional commands.
Direct absolute, relative, flag-value, and symlinked path arguments outside
allowed roots are rejected for both command execution modes.

### Filesystem

- allowed path works;
- `../` escape rejected;
- symlink escape rejected;
- outside-root absolute path rejected;
- bounded read enforced;
- failed `expected_sha256` causes no write;
- incorrect `expected_occurrences` causes no replacement.

### Processes

- start;
- output read;
- stdin write;
- normal exit;
- timeout;
- explicit stop;
- child processes terminated;
- maximum process count enforced.

### Resource bounds

- stdout truncation;
- stderr truncation;
- directory result bound;
- file read bound.

---

## 25. Definition of Done — v0.1

Rivet v0.1 is complete when:

1. It builds as one native Rust binary on macOS, Linux, and Windows.
2. It starts as an MCP stdio server.
3. `rivet.toml` completely defines available CLI commands.
4. Adding Cargo, UV, Git, or another CLI requires no Rust code change.
5. `list_commands` exposes the current registry.
6. `run_command` executes only registered commands.
7. Restricted commands validate their first argument against `allowed_subcommands`.
8. No shell interpreter is involved.
9. Filesystem and cwd cannot escape configured roots.
10. File reading and command output are bounded.
11. Long-running processes support start/read/input/stop.
12. Timeout/stop terminates descendants on all supported platforms.
13. Configuration errors fail fast at startup.
14. Full test suite passes.
15. Release binary size and idle memory are measured and recorded.
16. No command-specific abstraction, network server, database, plugin architecture, or speculative framework exists.
