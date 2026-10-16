# MCP tools

Rivet exposes these tools through the native Secure MCP Tunnel. Paths and command working directories must be absolute and within the single workspace returned by `list_roots`. The [built-in limits](configuration.md#resource-limits) apply to every request.

| Tool | Purpose |
| --- | --- |
| `list_roots` | Return the filesystem roots selected at startup. |
| `list_commands` | Return registered command names and resolved executable paths. |
| `run_command` | Submit a registered command and return its managed process ID immediately. |
| `run_commands` | Submit 1–8 independent registered commands in one MCP request. |
| `start_process` | Start an interactive registered process with stdin available. |
| `list_processes` | Return one snapshot containing all retained running and completed processes. |
| `read_process_output` | Read captured stdout and stderr from byte offsets. |
| `send_process_input` | Write text to a running interactive process stdin. |
| `stop_process` | Stop a running process group and wait for completion. |
| `list_directory` | List up to 1000 entries under an allowed root. |
| `read_file` | Read up to 4 MiB from a UTF-8 file. |
| `read_files` | Read 1–8 UTF-8 file slices, up to 4 MiB total per request. |
| `write_file` | Create, overwrite, or append UTF-8 text. |
| `replace_text` | Replace exact text after checking the expected match count. |

## Commands

`run_command` and `start_process` accept `command`, `args`, `cwd`, and optional `timeout_ms`. The command name must be registered at session startup. Rivet treats `args` as opaque argv and does not interpret a first argument or shell syntax. MCP requests cannot inject environment variables.

`run_command` never waits for the executable to finish. It returns:

```json
{"process_id":123}
```

The child is already registered in the fixed process table before the response is returned. `run_commands` performs the same submit operation for 1–8 commands and returns one ordered result per input command.

`start_process` uses the same registry but keeps stdin available for `send_process_input`.

## Processes

`list_processes` is the single process-state API. It takes no arguments and returns `running` and `completed` arrays. Captured text is never included in this snapshot:

```json
{
  "running": [
    {
      "process_id": 123,
      "command": "cargo",
      "duration_ms": 820,
      "stdout_bytes": 0,
      "stderr_bytes": 0,
      "truncated": false
    }
  ],
  "completed": [
    {
      "process_id": 122,
      "command": "rg",
      "duration_ms": 64,
      "exit_code": 0,
      "stopped": false,
      "timed_out": false,
      "stdout_bytes": 12,
      "stderr_bytes": 0,
      "truncated": false
    }
  ]
}
```

Rows include process IDs, command names, duration, status, and stdout/stderr byte counts. Read text only with `read_process_output`. There is no completion cursor, ready queue, long-poll parameter, or per-process status wait API.

`read_process_output` accepts `process_id`, optional `stdout_offset`, `stderr_offset`, and `limit`. It returns the current state and the requested stdout/stderr slices. It never waits for state or output changes.

Offsets default to zero. `limit` defaults to 2 MiB and applies separately to each stream; a supplied limit must be positive and at most 2 MiB. `send_process_input` accepts at most 4 MiB of UTF-8 text per call and uses the 2-minute default timeout for the stdin write. Interactive processes use pipes rather than a terminal/PTY.

Process state exists only while the Rivet server is running. The server accepts at most 8 running managed children. The registry retains 32 fixed slots. When all 32 are occupied, the oldest completed process is evicted; running entries are never evicted. Only processes started through Rivet consume registry slots; descendants created by those processes are not counted.

`stop_process` terminates the whole process group and returns after the process reaches completed state. Server shutdown applies the same process-group termination semantics to every running slot.

## Files

`list_directory` accepts an absolute `path` and optional `depth` and `max_entries`. Depth must be 1–16; `max_entries` must be 1–1000. Each entry has a path and a type (`file`, `directory`, or `symlink`). The response includes `truncated` when more entries exist than the limit allows.

`read_file` accepts an absolute `path` and optional byte `offset` and `limit`. The read limit must be 1–4,194,304 bytes; offsets that split a UTF-8 character are rejected.

`read_files` accepts `files`, containing 1–8 `read_file` requests. Results preserve input order and report success or error independently for each entry. The sum of requested read limits cannot exceed 4 MiB. Each omitted read limit defaults to 4 MiB; requests containing multiple files need explicit limits.

`write_file` accepts `path`, UTF-8 `content`, `mode` (`create`, `overwrite`, or `append`), and optional `expected_sha256`. Each supplied content string is limited to 4 MiB; repeated appends can produce a larger file. When supplied, the hash must match the existing file before modification, so it cannot be used with `create` or to append to a missing file.

`replace_text` accepts `path`, `old`, `new`, `expected_occurrences`, and optional `expected_sha256`. The file is changed only when the number of exact matches equals `expected_occurrences` and the optional hash matches.

Tool errors include a `code`, a human-readable `message`, and a `details` object (empty when no extra details apply). The implementation returns `INVALID_REQUEST`, `PATH_DENIED`, `COMMAND_NOT_FOUND`, `COMMAND_TIMEOUT`, `PROCESS_NOT_FOUND`, `PROCESS_LIMIT`, `OUTPUT_LIMIT`, `FILE_CHANGED`, `IO_ERROR`, `SPAWN_ERROR`, or `SANDBOX_ERROR`. Root and command-scope construction failures print `STARTUP_ERROR` to stderr; CLI parsing and later runtime errors have their own messages.
