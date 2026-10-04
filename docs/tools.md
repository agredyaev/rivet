# MCP tools

Rivet exposes these tools over MCP stdio. Paths and command working directories must be absolute and within an effective allowed root. Optional limits cannot exceed the values configured in [`rivet.toml`](configuration.md).

| Tool | Purpose |
| --- | --- |
| `list_roots` | Return effective allowed roots. |
| `list_commands` | Return registered commands and their argument policy. |
| `run_command` | Submit a registered command and return its managed process handle immediately. |
| `start_process` | Start a registered long-running command and return its process ID. |
| `list_processes` | List retained managed processes for recovery after an interrupted request. |
| `list_ready_processes` | List completed process IDs by completion sequence without consuming them. |
| `read_process_output` | Read captured stdout and stderr from byte offsets. |
| `send_process_input` | Write text to a running process stdin. |
| `stop_process` | Stop a running process and its descendants. |
| `list_directory` | List a bounded number of entries under an allowed root. |
| `read_file` | Read a byte range from a UTF-8 file. |
| `write_file` | Create, overwrite, or append UTF-8 text. |
| `replace_text` | Replace exact text after checking the expected match count. |

## Commands

`run_command` and `start_process` accept `command`, `args`, `cwd`, optional `timeout_ms`, and optional environment overrides in `env`. The command name must be registered. Restricted commands authorize the first argument using `allowed_subcommands`. A missing timeout uses `default_timeout_ms` for `run_command` and `max_timeout_ms` for `start_process`.

`run_command` does not wait for process completion. It returns `state = "submitted"` and `process_id` after spawn and supervisor handoff. Completion is published by the supervisor when the operating system reports process exit.

## Processes

`start_process` returns `process_id`. `list_processes` returns retained running and completed processes with their IDs, command names, status, and duration. `list_ready_processes` returns completed process metadata after an optional completion cursor. The cursor is non-destructive, so the same request can be retried safely. `read_process_output` accepts `process_id`, optional `stdout_offset`, `stderr_offset`, and `limit`; offsets are byte positions. The response includes text, next offsets, total byte counts, and truncation state. `send_process_input` accepts `process_id` and UTF-8 `text`. `stop_process` accepts `process_id`.

Process state exists only while the Rivet server is running. Live child processes consume `max_running_processes` capacity. Completed entries move to the ready queue and no longer consume execution capacity. The retained registry stays bounded; Rivet evicts the oldest ready entry when it needs space for a new process.

## Files

`list_directory` accepts an absolute `path` and optional `depth` and `max_entries`. Depth must be 1–16; the entry count cannot exceed `max_directory_entries`. Each entry has a path and a type (`file`, `directory`, or `symlink`). The response includes `truncated` when more entries exist than the limit allows.

`read_file` accepts an absolute `path` and optional byte `offset` and `limit`. Reads are bounded by `max_file_read_bytes`; offsets that split a UTF-8 character are rejected.

`write_file` accepts `path`, UTF-8 `content`, `mode` (`create`, `overwrite`, or `append`), and optional `expected_sha256`. When supplied, the hash must match the existing file before modification, so it cannot be used with `create` or to append to a missing file.

`replace_text` accepts `path`, `old`, `new`, `expected_occurrences`, and optional `expected_sha256`. The file is changed only when the number of exact matches equals `expected_occurrences` and the optional hash matches.

Tool errors include a `code`, a human-readable `message`, and optional `details`. The implementation returns `INVALID_REQUEST`, `PATH_DENIED`, `COMMAND_NOT_FOUND`, `COMMAND_DENIED`, `SUBCOMMAND_DENIED`, `COMMAND_TIMEOUT`, `PROCESS_NOT_FOUND`, `PROCESS_LIMIT`, `OUTPUT_LIMIT`, `FILE_CHANGED`, `IO_ERROR`, or `SPAWN_ERROR`. CLI configuration failures print `CONFIG_ERROR` to stderr.
