# Security model

Native `rivet session` is an outbound-only MCP tunnel client. It does not open an inbound listener.

## Native session boundary

```text
Current workspace      READ / WRITE
Sandbox HOME + tmp     READ / WRITE
Required toolchains    READ / EXECUTE
System runtime         READ / EXECUTE

Real user HOME         DENIED
~/.ssh                 DENIED
~/.aws                 DENIED
Sibling projects       DENIED
Other host files       DENIED

Network:
  rivet session             DENIED for child
  rivet session --network   ENABLED for dependency downloads

Rivet itself:
  outbound HTTPS to OpenAI  REQUIRED
  inbound listener          NONE
```

- The current directory is the only workspace root.
- Rivet filesystem tools use capability-based paths and cannot leave that workspace.
- Child commands are OS-sandboxed before execution.
- macOS uses `/usr/bin/sandbox-exec` (Seatbelt).
- Linux and WSL2 use `bubblewrap`. Ubuntu 24.04's AppArmor restriction on unprivileged user namespaces requires the distro `bwrap-userns-restrict` profile; Rivet never disables that restriction globally.
- The workspace is writable.
- Common system and toolchain locations are read-only.
- Other user data is not exposed to the child sandbox.
- Child processes can spawn descendants, but the sandbox policy is inherited.
- Child processes cannot signal processes outside their sandbox/process namespace.
- Child network access is disabled by default. `rivet session --network` explicitly enables child networking for dependency downloads and similar workflows; the filesystem sandbox remains enforced. On macOS the Seatbelt policy grants outbound networking only. On Linux/WSL2 this opt-in uses the host network namespace, so child processes may also bind listeners. Rivet itself retains outbound HTTPS access to the OpenAI tunnel.
- If the platform sandbox is unavailable, native `session` fails closed instead of running commands unsandboxed.

Command allowlists and the OS sandbox solve different problems: the allowlist controls which top-level programs MCP may request; the sandbox limits what allowed programs and any code they execute can do to the host.

## Credentials

The tunnel runtime API key is entered with terminal echo disabled, is kept in process memory, and is sent only as the Bearer authorization header. HTTP redirects are disabled. Production uses `https://api.openai.com`; overriding the control-plane URL requires the explicit unsafe test opt-in.

## Legacy stdio mode

The legacy `serve`/configuration commands remain available for local MCP clients. Their configured roots and argument checks are compatibility policy, not the native tunnel sandbox.
