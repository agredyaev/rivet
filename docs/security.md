# Security model

Rivet is a local command gateway. Its configuration controls which commands an MCP client can request and which paths Rivet's own filesystem tools can access.

## What Rivet restricts

- Commands must be registered by name.
- Restricted commands must use an allowed first argument.
- Arguments are passed directly to the configured executable; Rivet does not start a shell implicitly.
- Rivet resolves command arguments against `cwd`. It also checks values after `=`, `@path` arguments, and single-dash forms such as `-I/path`; detected paths outside the roots are rejected.
- File operations and command working directories must stay within effective roots.
- Child processes receive only configured inherited environment variables, plus explicitly allowed request overrides.
- Command time, output, file reads, directory listings, and concurrent process count are bounded by configuration.

## What Rivet does not restrict

Allowed roots do not restrict filesystem access by a launched program. A registered executable runs with the Rivet user's permissions and can read or write any path that account can access. Argument checks do not sandbox the executable. Register only executables you trust.

Rivet communicates over stdin/stdout and does not listen on a network port. A tunnel gives its connected MCP client access to Rivet's configured tools. Restrict tunnel access and the command registry to the intended users and operations.

## Configuration checklist

1. Set roots to the directories used by file tools and command working directories.
2. For restricted commands, list permitted first arguments in `allowed_subcommands`; otherwise set `allow_any_args = true`.
3. Do not register shells or interpreters with unrestricted arguments unless that is intentional.
4. Do not commit `rivet.toml`; it contains local paths and allowed commands.
5. Child commands run with the Rivet user's OS permissions.
