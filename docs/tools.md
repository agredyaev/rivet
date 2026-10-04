# MCP tools

Rivet exposes these tools over MCP stdio. Paths and command working directories must be absolute and within an effective allowed root. Optional limits cannot exceed the values configured in [`rivet.toml`](configuration.md).

| Tool | Purpose |
| --- | --- |
| `list_roots` | Return effective allowed roots. |
| `list_commands` | Return registered commands and their argument policy. |
| `run_command` | Run a registered command briefly; return a managed process handle if it is still running. |
| `start_process` | Start a registered long-running command and return its process ID. |
| `list_processes` | List retained managed processes for recovery after an interrupted request. |
| `read_process_output` | Read captured stdout and stderr from byte offsets. |
| `send_process_input` | Write text to a running process stdin. |
| `stop_process` | Stop a running process and its descendants. |
| `list_directory` | List a bounded number of entries under an allowed root. |
| `read_file` | Read a byte range from a UTF-8 file. |
| `write_file` | Create, overwrite, or append UTF-8 text. |
| `replace_text` | Replace exact text after checking the expected match count. |

## Commands

`run_command` and `start_process` accept `command`, `args`, `cwd`, optional `timeout_ms`, and optional environment overrides in `env`. The command name must be registered. Restricted commands authorize the first argument using `allowed_subcommands`. A missing timeout uses `default_timeout_ms` for `run_command` and `max_timeout_ms` for `start_process`.

`run_command` waits up to `foreground_wait_ms`. A completed command returns `state = "completed"` with its exit status and bounded output. A command that still runs returns `state = "running"`, `process_id`, current output, byte offsets, and duration. The managed process continues independently of that MCP request.

## Processes

`start_process` returns `process_id`. `list_processes` returns retained running and completed processes with their IDs, command names, status, and duration. Use it to recover a handle after an interrupted MCP response. `read_process_output` accepts `process_id`, optional `stdout_offset`, `stderr_offset`, and `limit`; offsets are byte positions. The response includes text, next offsets, total byte counts, and truncation state. `send_process_input` accepts `process_id` and UTF-8 `text`. `stop_process` accepts `process_id`.

Process state exists only while the Rivet server is running. Live child processes consume `max_running_processes` capacity. `start_process` and detached `run_command` jobs use background capacity, while `foreground_process_reserve` stays available for short `run_command` calls. Completed retained entries do not consume live-process capacity. Rivet evicts the oldest completed entry when retained state reaches the configured process bound.

## Files

`list_directory` accepts an absolute `path` and optional `depth` and `max_entries`. Depth must be 1–16; the entry count cannot exceed `max_directory_entries`. Each entry has a path and a type (`file`, `directory`, or `symlink`). The response includes `truncated` when more entries exist than the limit allows.

`read_file` accepts an absolute `path` and optional byte `offset` and `limit`. Reads are bounded by `max_file_read_bytes`; offsets that split a UTF-8 character are rejected.

`write_file` accepts `path`, UTF-8 `content`, `mode` (`create`, `overwrite`, or `append`), and optional `expected_sha256`. When supplied, the hash must match the existing file before modification, so it cannot be used with `create` or to append to a missing file.

`replace_text` accepts `path`, `old`, `new`, `expected_occurrences`, and optional `expected_sha256`. The file is changed only when the number of exact matches equals `expected_occurrences` and the optional hash matches.

Tool errors include a `code`, a human-readable `message`, and optional `details`. The implementation returns `INVALID_REQUEST`, `PATH_DENIED`, `COMMAND_NOT_FOUND`, `COMMAND_DENIED`, `SUBCOMMAND_DENIED`, `COMMAND_TIMEOUT`, `PROCESS_NOT_FOUND`, `PROCESS_LIMIT`, `OUTPUT_LIMIT`, `FILE_CHANGED`, `IO_ERROR`, or `SPAWN_ERROR`. CLI configuration failures print `CONFIG_ERROR` to stderr.
