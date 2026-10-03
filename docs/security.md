# Security model

Rivet is a local command gateway. Startup arguments define which programs an MCP client can request and which paths Rivet's own filesystem tools can access.

## What Rivet restricts

- Programs must be allowed at server startup with `--allow-command`.
- Restricted programs must use an allowed first argument passed with `--allow-subcommand`.
- Filesystem roots are supplied at server startup with `--root`.
- Arguments are passed directly to the configured executable; Rivet does not start a shell implicitly.
- Rivet resolves command arguments against `cwd`. It also checks values after `=`, `@path` arguments, and single-dash forms such as `-I/path`; detected paths outside the roots are rejected.
- File operations and command working directories must stay within effective roots.
- Child processes receive only configured inherited environment variables, plus explicitly allowed request overrides.
- Command time, output, file reads, directory listings, and concurrent process count are bounded by configuration.

## What Rivet does not restrict

Allowed roots do not restrict filesystem access by a launched program. A registered executable runs with the Rivet user's permissions and can read or write any path that account can access. Argument checks do not sandbox the executable. Register only executables you trust.

Rivet communicates over stdin/stdout and does not listen on a network port. A tunnel gives its connected MCP client access to Rivet's configured tools. Restrict tunnel access and the startup allowlist to the intended users and operations.

## Session scope checklist

1. Pass only the roots needed for the current session with `--root`.
2. Pass each allowed executable and permitted first argument with `--allow-command` and `--allow-subcommand`.
3. Do not register shells or interpreters with unrestricted arguments unless that is intentional.
4. Rivet keeps this scope in the temporary local tunnel profile and removes the profile when the session ends.
5. Child commands run with the Rivet user's OS permissions.
