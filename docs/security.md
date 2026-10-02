# Security model

Rivet is a local command gateway. Its configuration controls which commands an MCP client can request and which paths Rivet's own filesystem tools can access.

## What Rivet restricts

- Commands must be registered by name.
- Restricted commands must use an allowed first argument.
- Arguments are passed directly to the configured executable; Rivet does not start a shell implicitly.
- Rivet checks path-like command arguments and rejects paths outside configured roots when they can be identified.
- File operations and command working directories must stay within effective roots.
- Child processes receive only configured inherited environment variables, plus explicitly allowed request overrides.
- Command time, output, file reads, directory listings, and concurrent process count are bounded by configuration.

## What Rivet does not restrict

Allowed roots are not an operating-system sandbox. A registered executable runs with the Rivet user's permissions. It can access resources through its own behavior, configuration, environment, or arguments that Rivet cannot identify as paths. Do not register untrusted programs or grant commands broader access than the MCP client should have.

Rivet communicates over stdin/stdout and does not listen on a network port. If a tunnel exposes it to a remote client, that client can use the configured tools and permissions. Protect the tunnel and limit the command registry accordingly.

## Safer setup

1. Set roots to the smallest directories needed.
2. Prefer explicit `allowed_subcommands` over `allow_any_args` when practical.
3. Do not register shells or interpreters with unrestricted arguments unless that is intentional.
4. Keep `rivet.toml` private and review environment variables passed to child processes.
5. Run Rivet under an account whose OS permissions match the access you intend to grant.
