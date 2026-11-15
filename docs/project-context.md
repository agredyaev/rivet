# Persisted project context

Rivet stores project context as immutable generations.

- `project_context` reads the latest committed generation or one specified retained generation.
- `project_checkpoint` writes the next generation from a complete replacement snapshot.

The MCP server instructions require `project_context` before the first workspace file operation or registered command in a task. They permit `project_checkpoint` only when at least one stored record is added, removed, or changed. Rivet returns context only from `project_context`; it does not prepend context to other MCP requests.

## Snapshot format

The tool payload is UTF-8 text.

The first line is:

```text
GEN<TAB>generation
```

Each following line is:

```text
kind<TAB>text
```

Record kinds are:

| Code | Store this statement |
| --- | --- |
| `G` | an outcome or requirement that a later task must still satisfy |
| `R` | a component, ownership, or interface boundary that later changes must not cross unless the boundary is changed |
| `C` | an architecture or implementation rule that later changes must satisfy |
| `D` | a project decision already selected and still in force |
| `F` | a fact confirmed by a test, tool, or inspection that is not present in source files and is not directly retrievable from Git or `rg` |
| `T` | an unfinished action intentionally deferred to a later task or chat |

Do not store hypotheses, raw command or test output, current build/test status, or facts present in source files or directly retrievable from Git or `rg`.

Record text must be non-empty and cannot contain a newline or carriage return.

A checkpoint snapshot replaces the previous snapshot; it is not a delta. Include every existing record whose statement is still true and in force. Remove records whose statements are no longer true or in force. Replace records whose stored statement changed.

## Task lifecycle

For a task that will use Rivet workspace files or registered commands:

1. Call `project_context` with `generation` omitted before the first workspace file operation or registered command.
2. Perform the task.
3. If no stored record must be added, removed, or changed, do not call `project_checkpoint`.
4. If at least one stored record must change, send the complete updated snapshot to `project_checkpoint`.

Passing `generation` to `project_context` is only for reading that historical generation.

## Generation checks

When `project_checkpoint` receives `GEN<TAB>N`, Rivet compares `N` with the latest committed generation.

- If they match, Rivet writes generation `N + 1`.
- If they do not match, the tool returns `CONTEXT_CHANGED<TAB>message` and writes nothing.
- After `CONTEXT_CHANGED`, call `project_context` without `generation`, apply the intended additions, removals, and replacements to that snapshot, and retry.

Generation `0` means no project-context generation has been committed. `project_context` returns `GEN<TAB>0` when no committed generation exists.

## Storage paths

For a Git workspace, Rivet stores context under the Git common directory:

```text
<git-common-dir>/rivet/context/
```

For a non-Git workspace:

```text
<workspace>/.rivet/context/
```

Linked Git worktrees that use the same Git common directory therefore use the same Rivet project-context directory.

The directory contains:

```text
context.idx
context.000000.bin
context.000001.bin
...
```

`context.idx` contains fixed 16-byte generation entries. Data files are append-only segments. A snapshot never crosses a segment boundary.

## Limits

| Limit | Value |
| --- | ---: |
| records per generation | 128 |
| total record text per generation | 32 KiB |
| data segment size | 1 MiB |
| index entry size | 16 bytes |

A generation lookup reads its fixed index entry directly; it does not scan earlier generations.

## Write and recovery behavior

A checkpoint writes the snapshot frame to a segment and syncs that segment before it appends the generation index entry. The index entry is the commit point for the generation.

Readers ignore a partial trailing index entry. Before the next mutation, Rivet truncates that partial index tail. A complete frame written without a committed index entry is an orphan tail; Rivet truncates that tail before a later append to the same segment.

If a committed index entry references malformed data, `project_context` returns `STORAGE_ERROR<TAB>message`. It does not fall back to an older generation.

## Historical reads and retention

`project_context` with `generation=N` reads generation `N` directly from its index entry when its segment is present.

The context core contains segment-pruning logic, but this version has no MCP tool, CLI command, or runtime policy that invokes it. Normal MCP use therefore does not delete old context segments automatically.

If a retained generation points to a segment that has been removed, `project_context` returns `GENERATION_EXPIRED<TAB>message`. If a segment that should be present is missing or malformed, it returns `STORAGE_ERROR<TAB>message`.

## Concurrency

Checkpoint mutations within one Rivet process use the same mutation lock as other Rivet file mutations. Reads do not take that lock.

Concurrent writes to the same project-context directory from multiple Rivet processes are not supported.
