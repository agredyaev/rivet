# Security model

Native `rivet session` is an outbound-only MCP tunnel client. It does not open an inbound listener.

## Native session boundary

```text
Current workspace               READ / WRITE
Sandbox HOME + tmp              READ / WRITE
Required toolchains             READ / EXECUTE
System runtime                  READ / EXECUTE
Linked-worktree Git metadata    READ / WRITE by git only

Host reads                      Backend rules listed below

Network:
  rivet session             DENIED for child
  rivet session --network   ENABLED for child

Rivet itself:
  outbound HTTPS to OpenAI  REQUIRED
  inbound listener          NONE
```

- The current directory is the only workspace root.
- Rivet filesystem tools use capability-based paths and cannot leave that workspace.
- Child commands are OS-sandboxed before execution.
- When the workspace is a linked Git worktree, commands launched as the `git` executable and their descendants receive read/write access to the worktree Git directory and its Git common directory. The common directory contains refs and the repository object database, so `git` can read repository history stored there. Rivet does not grant this extra access to file tools or commands launched as other executables.
- macOS uses `/usr/bin/sandbox-exec` (Seatbelt).
- Linux and WSL2 use `bubblewrap`. Ubuntu 24.04's AppArmor restriction on unprivileged user namespaces requires the distro `bwrap-userns-restrict` profile; Rivet never disables that restriction globally.
- The workspace is writable.
- Host toolchain paths are read-only: `~/.cargo/bin`, `~/.cargo/registry`, `~/.rustup`, `~/.local/bin`, and `~/.local/share/uv`, when present.
- User home data is hidden except for the selected workspace, sandbox cache, permitted executables, and listed toolchain paths. A workspace containing secrets makes those files available to the session.
- Host-file read access differs by backend. macOS permits system locations including `/usr`, `/Library`, `/etc`, and Homebrew paths. Linux starts with a read-only host root and masks `/home`, `/root`, `/mnt`, `/media`, `/run`, the actual user home, and `/tmp`, then exposes the workspace, sandbox cache, executable, and listed toolchain paths. Unmasked host locations remain visible read-only, subject to host file permissions.
- The per-workspace sandbox home, temporary directory, and Cargo home persist under `~/Library/Caches/rivet` on macOS or `~/.cache/rivet` on Linux. Session credentials and command selections are not saved there.
- Child processes can spawn descendants, but the sandbox policy is inherited.
- Child processes cannot signal processes outside their sandbox/process namespace.
- Child network access is disabled by default. `rivet session --network` explicitly enables child networking for dependency downloads, package-index access, and remote build services; the filesystem sandbox remains enforced. On macOS the Seatbelt policy grants outbound networking only. On Linux/WSL2 this opt-in uses the host network namespace, so child processes may also bind listeners. Rivet itself retains outbound HTTPS access to the OpenAI tunnel.
- If the platform sandbox is unavailable, native `session` fails closed instead of running commands unsandboxed.

Command allowlists and the OS sandbox solve different problems: the allowlist controls which top-level programs MCP may request; the sandbox limits what allowed programs and any code they execute can do to the host.

## Credentials

When credentials are entered interactively, Rivet disables terminal echo for both the tunnel ID and the runtime API key. Rivet keeps both values in process memory and does not persist them. The runtime API key is sent only as the Bearer authorization header. HTTP redirects are disabled. Production uses `https://api.openai.com`; overriding the control-plane URL requires the explicit unsafe test opt-in.

## Direct stdio mode

The stdio commands (`serve`, `doctor`, `commands`) remain available for local MCP clients and use built-in resource limits and environment policies. File tools and command working directories are confined to the selected roots. Argument path checks do not isolate the executed program: stdio children run with the host user's OS permissions and receive no native-session sandbox.
