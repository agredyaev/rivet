# MCP tools

Rivet exposes these tools through both the native tunnel and MCP stdio. Paths and command working directories must be absolute and within the effective workspace root. All modes use the [built-in limits](configuration.md#resource-limits).

| Tool | Purpose |
| --- | --- |
| `list_roots` | Return effective allowed roots. |
| `list_commands` | Return registered commands and their argument policy. |
| `run_command` | Submit a registered command and return its managed process ID immediately. |
| `run_commands` | Submit 1–8 independent registered commands in one MCP request. |
| `start_process` | Start an interactive registered process with stdin available. |
| `list_processes` | Return one snapshot containing all retained running and completed processes. |
| `read_process_output` | Read captured stdout and stderr from byte offsets. |
| `send_process_input` | Write text to a running interactive process stdin. |
| `stop_process` | Stop a running process group and wait for completion. |
| `list_directory` | List a bounded number of entries under an allowed root. |
| `read_file` | Read a byte range from a UTF-8 file. |
| `read_files` | Read up to 8 bounded UTF-8 file slices in one MCP request. |
| `write_file` | Create, overwrite, or append UTF-8 text. |
| `replace_text` | Replace exact text after checking the expected match count. |

## Commands

`run_command` and `start_process` accept `command`, `args`, `cwd`, optional `timeout_ms`, and optional environment overrides in `env`. The command name must be registered. Restricted commands authorize the first argument using `allowed_subcommands`.

`run_command` never waits for the executable to finish. It returns:

```json
{"process_id":123}
```

The child is already registered in the fixed process table before the response is returned. `run_commands` performs the same submit operation for 1–8 commands and returns one ordered result per input command.

`start_process` uses the same registry but keeps stdin available for `send_process_input`.

## Processes

The managed-process model is fixed and bounded:

```text
MCP submit
   |
   v
+---------------------------+
| ProcessTable              |
| 32 retained slots         |
| max 10 running children   |
| canonical state in Core   |
+-------------+-------------+
              |
       state transitions
              v
         state_event
      notification only

Captured output is stored separately:
32 process slots x (stdout + stderr) bounded buffers.
`list_processes` returns metadata and byte counts only.
`read_process_output` returns bounded text slices by offset.
```

`list_processes` is the single process-state API. It takes no arguments and returns compact `running` and `completed` arrays. Captured text is never included in this snapshot:

```json
{
  "running": [
    {
      "process_id": 123,
      "command": "cargo",
      "duration_ms": 820,
      "stdout_bytes": 0,
      "stderr_bytes": 0
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
      "stderr_bytes": 0
    }
  ]
}
```

Rows include only control-plane metadata and stdout/stderr byte counts. Read text only with `read_process_output`. There is no completion cursor, ready queue, long-poll parameter, or per-process status wait API.

`read_process_output` accepts `process_id`, optional `stdout_offset`, `stderr_offset`, and `limit`. It returns the current state plus bounded output slices. It never waits for state or output changes.

Process state exists only while the Rivet server is running. Up to `max_running_processes` children may be active (maximum 10). The registry retains 32 fixed slots. When all 32 are occupied, the oldest completed process is evicted; running entries are never evicted.

`stop_process` terminates the whole process group and returns after the process reaches completed state. Server shutdown applies the same process-group termination semantics to every running slot.

## Files

`list_directory` accepts an absolute `path` and optional `depth` and `max_entries`. Depth must be 1–16; the entry count cannot exceed `max_directory_entries`. Each entry has a path and a type (`file`, `directory`, or `symlink`). The response includes `truncated` when more entries exist than the limit allows.

`read_file` accepts an absolute `path` and optional byte `offset` and `limit`. Reads are bounded by `max_file_read_bytes`; offsets that split a UTF-8 character are rejected.

`read_files` accepts `files`, containing 1–8 `read_file` requests. Results preserve input order and report success or error independently for each entry. The sum of requested read limits cannot exceed `max_file_read_bytes`, so batching does not expand the single-request file I/O and memory bound.

`write_file` accepts `path`, UTF-8 `content`, `mode` (`create`, `overwrite`, or `append`), and optional `expected_sha256`. When supplied, the hash must match the existing file before modification, so it cannot be used with `create` or to append to a missing file.

`replace_text` accepts `path`, `old`, `new`, `expected_occurrences`, and optional `expected_sha256`. The file is changed only when the number of exact matches equals `expected_occurrences` and the optional hash matches.

Tool errors include a `code`, a human-readable `message`, and optional `details`. The implementation returns `INVALID_REQUEST`, `PATH_DENIED`, `COMMAND_NOT_FOUND`, `COMMAND_DENIED`, `SUBCOMMAND_DENIED`, `COMMAND_TIMEOUT`, `PROCESS_NOT_FOUND`, `PROCESS_LIMIT`, `OUTPUT_LIMIT`, `FILE_CHANGED`, `IO_ERROR`, `SPAWN_ERROR`, or `SANDBOX_ERROR`. CLI startup validation failures print `STARTUP_ERROR` to stderr.
