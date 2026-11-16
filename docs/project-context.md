# Persisted project context

Rivet stores project context as immutable generations.

- `project_context` reads the latest committed generation or one specified retained generation.
- `project_checkpoint` writes the next generation from a complete replacement snapshot.

Both tools take an explicit absolute `project` path inside the Rivet workspace. The MCP server instructions require `project_context` for that project before the first workspace file operation or registered command for the project in a task. They permit `project_checkpoint` for that project only when at least one stored record is added, removed, or changed. Separate project directories have separate generations. Rivet returns context only from `project_context`; it does not prepend context to other MCP requests.

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

For a task that will use one or more projects under the Rivet workspace:

1. Identify the absolute root path of each project involved.
2. Before the first workspace file operation or registered command for a project, call `project_context` with that `project` path and `generation` omitted.
3. Perform the task.
4. If no stored record for a project must be added, removed, or changed, do not call `project_checkpoint` for it.
5. If at least one stored record for a project must change, send that project's absolute `project` path and complete updated snapshot to `project_checkpoint`.

Passing `generation` to `project_context` is only for reading that historical generation.

## Generation checks

When `project_checkpoint` receives `GEN<TAB>N`, Rivet compares `N` with the latest committed generation.

- If they match, Rivet writes generation `N + 1`.
- If they do not match, the tool returns `CONTEXT_CHANGED<TAB>message` and writes nothing.
- After `CONTEXT_CHANGED`, call `project_context` without `generation`, apply the intended additions, removals, and replacements to that snapshot, and retry.

Generation `0` means no project-context generation has been committed. `project_context` returns `GEN<TAB>0` when no committed generation exists. The first checkpoint must contain at least one record; `GEN<TAB>0` by itself is rejected.

## Storage paths

For a project whose explicit root contains a `.git` directory or worktree marker, Rivet stores context under that project's Git common directory:

```text
<git-common-dir>/rivet/context/
```

For a non-Git project:

```text
<project>/.rivet/context/
```

Linked Git worktrees that use the same Git common directory therefore use the same Rivet project-context directory. Sibling projects under one Rivet workspace use different context directories and generation histories.

The directory contains:

```text
context.idx
context.000000.bin
context.000001.bin
...
context.pruned
```

`context.idx` contains fixed 16-byte generation entries. Data files are append-only segments. A snapshot never crosses a segment boundary. When retention removes one or more complete prefix segments, `context.pruned` records the first segment that was intentionally retained so later reads can distinguish expiration from unexpected storage loss.

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

If pruning removed the segment for a requested generation, `project_context` returns `GENERATION_EXPIRED<TAB>message`. Retention persists a pruning watermark before deleting segments. A missing segment that is not covered by that watermark is treated as unexpected storage loss and returns `STORAGE_ERROR<TAB>message`; malformed retained data also returns `STORAGE_ERROR<TAB>message`.

## Concurrency

Checkpoint mutations within one Rivet process use the same mutation lock as other Rivet file mutations. Reads do not take that lock.

Concurrent writes to the same project-context directory from multiple Rivet processes are not supported.
