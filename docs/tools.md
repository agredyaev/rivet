# MCP tools

Rivet exposes these tools through both the native tunnel and MCP stdio. Paths and command working directories must be absolute and within the effective workspace root. Native `session` uses embedded limits; stdio mode takes limits from [`rivet.toml`](configuration.md).

| Tool | Purpose |
| --- | --- |
| `list_roots` | Return effective allowed roots. |
| `list_commands` | Return registered commands and their argument policy. |
| `run_command` | Run a registered command; complete fast commands inline and return a managed process ID for slower commands. |
| `run_commands` | Run up to 8 independent registered commands concurrently inside one MCP request. |
| `start_process` | Start a registered command asynchronously and return its process ID immediately. |
| `list_processes` | List retained managed processes for recovery after an interrupted request. |
| `list_ready_processes` | List completed processes by completion sequence and optionally include bounded stdout/stderr. |
| `read_process_output` | Read captured stdout and stderr from byte offsets, optionally waiting for new output or completion. |
| `send_process_input` | Write text to a running process stdin. |
| `stop_process` | Stop a running process and its descendants. |
| `list_directory` | List a bounded number of entries under an allowed root. |
| `read_file` | Read a byte range from a UTF-8 file. |
| `write_file` | Create, overwrite, or append UTF-8 text. |
| `replace_text` | Replace exact text after checking the expected match count. |

## Commands

`run_command` accepts `command`, `args`, `cwd`, optional `timeout_ms`, optional environment overrides in `env`, optional `wait_ms`, and optional `output_limit`. The default `wait_ms` is 250 ms and the maximum is 5000 ms. If the child finishes within that window, Rivet returns `state = "completed"`, status, and bounded stdout/stderr in the same MCP response. Otherwise Rivet returns `state = "submitted"` and `process_id`; the child continues independently of the MCP/tunnel request.

`run_commands` accepts 1–8 independent command requests plus shared optional `wait_ms` and `output_limit`. Rivet applies the same allowlist, sandbox, timeout, and process-capacity checks to every command. It starts accepted commands concurrently inside one MCP request and returns one ordered result per input command. Use this tool when independent commands would otherwise require several serialized calls to the same MCP server.

`start_process` accepts the command fields without the fast wait and returns `process_id` immediately. It keeps stdin available for interactive work.

## Processes

`start_process` returns `process_id`. `list_processes` returns retained running and completed processes with their IDs, command names, status, and duration. `list_ready_processes` accepts optional `after_sequence`, `limit`, and `output_limit`. Set `output_limit` to include that many bounded bytes from each ready process stdout and stderr in the same MCP response. Omit it for metadata only. The cursor is non-destructive, so the same request can be retried safely. `read_process_output` accepts `process_id`, optional `stdout_offset`, `stderr_offset`, `limit`, and `wait_ms`; offsets are byte positions. `wait_ms` may be 0–30000. When no new output is available and the process is still running, Rivet waits on process activity or completion instead of polling internally. Use it for output beyond the ready-response prefix. `send_process_input` accepts `process_id` and UTF-8 `text`. `stop_process` accepts `process_id`.

Process state exists only while the Rivet server is running. Live child processes consume `max_running_processes` capacity. Completed entries move to the ready queue and no longer consume execution capacity. The retained registry stays bounded; Rivet evicts the oldest ready entry when it needs space for a new process.

## Files

`list_directory` accepts an absolute `path` and optional `depth` and `max_entries`. Depth must be 1–16; the entry count cannot exceed `max_directory_entries`. Each entry has a path and a type (`file`, `directory`, or `symlink`). The response includes `truncated` when more entries exist than the limit allows.

`read_file` accepts an absolute `path` and optional byte `offset` and `limit`. Reads are bounded by `max_file_read_bytes`; offsets that split a UTF-8 character are rejected.

`write_file` accepts `path`, UTF-8 `content`, `mode` (`create`, `overwrite`, or `append`), and optional `expected_sha256`. When supplied, the hash must match the existing file before modification, so it cannot be used with `create` or to append to a missing file.

`replace_text` accepts `path`, `old`, `new`, `expected_occurrences`, and optional `expected_sha256`. The file is changed only when the number of exact matches equals `expected_occurrences` and the optional hash matches.

Tool errors include a `code`, a human-readable `message`, and optional `details`. The implementation returns `INVALID_REQUEST`, `PATH_DENIED`, `COMMAND_NOT_FOUND`, `COMMAND_DENIED`, `SUBCOMMAND_DENIED`, `COMMAND_TIMEOUT`, `PROCESS_NOT_FOUND`, `PROCESS_LIMIT`, `OUTPUT_LIMIT`, `FILE_CHANGED`, `IO_ERROR`, or `SPAWN_ERROR`. CLI configuration failures print `CONFIG_ERROR` to stderr.
