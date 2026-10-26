# Connect Rivet to ChatGPT

Rivet connects to ChatGPT through OpenAI Secure MCP Tunnel using a single native in-process transport.

## Architecture

The runtime path is:

```text
                         +----------------------+
                         |       ChatGPT        |
                         +----------+-----------+
                                    |
                                    | OpenAI Secure MCP Tunnel
                                    v
                         +----------------------+
                         |      OpenAI API      |
                         +----------+-----------+
                                    |
                             outbound HTTPS
                           poll / response only
                                    |
                                    v
+--------------------------------------------------------------+
|                           RIVET                              |
|                      single process                          |
|                                                              |
|  native tunnel -> in-process MCP dispatch                    |
|                         |                                    |
|               +---------+---------+                          |
|               |                   |                          |
|               v                   v                          |
|        filesystem tools      command tools                   |
|            cap-std            allowlisted exec               |
|               |                   |                          |
|               |                   v                          |
|               |             OS sandbox                       |
|               |        macOS: Seatbelt                       |
|               |        Linux/WSL2: bwrap                     |
|               |                   |                          |
|               |                   v                          |
|               |              child process                   |
+---------------+-------------------+--------------------------+
                |                   |
                +--------+----------+
                         v
              +------------------------+
              |   current workspace    |
              |       read/write       |
              +------------------------+
```

There is no `cloudflared`, `tunnel-client`, localhost proxy, second Rivet process, or native Windows backend in the native-session path.

Rivet advertises one MCP channel:

```json
{"version":2,"channels":[{"name":"main","stateless":true,"proc_affinity":true}]}
```

The channel accepts self-contained stateless MCP requests. Multiple ChatGPT conversations can use the same Rivet daemon concurrently; Rivet does not keep per-chat state. Those conversations share the daemon's command permissions, workspace files, and managed-process table.

## Start a session

From the workspace directory, run:

```sh
rivet session
```

Here `rivet` means the binary on `PATH` or its absolute path. The installer starts it directly without adding it to `PATH`; for manual downloads, use the extracted package's absolute `bin/rivet` path.

Rivet prints the workspace root, explicit child command paths, command names, resolved executable paths, child network policy, and selected environment-variable names. The built-in selector offers `uv`, `mkdir`, `rg`, `git`, `cargo`, and `make`; custom commands can be added explicitly. Interactive setup also asks whether child commands need network access and specific parent environment variables.

Rivet then asks for:

- the OpenAI tunnel ID;
- the tunnel runtime API key.

Both prompts disable terminal echo. Rivet does not save either value.

Create the tunnel and credentials before starting Rivet:

- [OpenAI Secure MCP Tunnel guide](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)
- [Platform tunnel settings](https://platform.openai.com/settings/organization/tunnels) — create or select the tunnel and copy its `tunnel_id`
- [Platform API keys](https://platform.openai.com/api-keys) — create the runtime API key for a role with **Tunnels Read + Use** permission

Creating or editing a tunnel requires **Tunnels Read + Manage**. Associate the tunnel with the target ChatGPT workspace as well as its Platform organization so it can appear when adding the plugin. Platform tunnel permissions and ChatGPT workspace access are separate. See the [official permissions and workspace guidance](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels#permissions-and-access).

For a non-interactive shell, export the variables before starting Rivet:

```sh
export CONTROL_PLANE_TUNNEL_ID="tunnel_0123456789abcdef0123456789abcdef"
export CONTROL_PLANE_API_KEY="sk-..."

rivet session \
  --allow-command git=git \
  --allow-command cargo=cargo \
  --allow-command-path "$HOME/.rustup" \
  --pass-env FRED_API_KEY \
  --network
```

To keep the real values out of shell history, read them without terminal echo and then export them. The following uses Bash's `read` options; run `bash` first if your shell is zsh:

```bash
read -rsp "Tunnel ID: " CONTROL_PLANE_TUNNEL_ID; echo
export CONTROL_PLANE_TUNNEL_ID
read -rsp "Runtime API key: " CONTROL_PLANE_API_KEY; echo
export CONTROL_PLANE_API_KEY

rivet session --allow-command git=git
```

Environment assignments can also be scoped to one Rivet invocation:

```sh
CONTROL_PLANE_TUNNEL_ID="tunnel_0123456789abcdef0123456789abcdef" \
CONTROL_PLANE_API_KEY="sk-..." \
rivet session \
  --allow-command git=git
```

Do not paste a real runtime API key into a command that your shell records in history.

Production uses `https://api.openai.com`. A different `CONTROL_PLANE_BASE_URL` is accepted only when `RIVET_ALLOW_UNSAFE_CONTROL_PLANE=1` is set explicitly for testing.

## Session lifecycle

`rivet session`:

1. canonicalizes the current directory as the single workspace root;
2. collects or accepts the command allowlist and canonicalizes explicit `--allow-command-path` grants;
3. captures explicit `--pass-env` values and the network capability;
4. validates the workspace, command paths, registered executables, and mandatory OS sandbox;
5. constructs the MCP runtime in the same process;
6. opens the OpenAI tunnel control-plane connection;
7. polls `/v1/tunnels/{tunnel_id}/poll`;
8. dispatches MCP requests directly to Rivet handlers;
9. posts terminal responses to `/v1/tunnels/{tunnel_id}/response`.

No profile file, downloaded tunnel binary, Cloudflare process, stdio MCP bridge, or localhost listener is created.

Press Ctrl+C to stop the daemon. Rivet then shuts down managed child processes through the existing `ProcessTable` lifecycle.

## Long-running commands

`run_command` never waits for an external executable to finish. It returns:

```json
{"process_id":123}
```

The child continues independently of the originating tunnel request, until its command timeout, an explicit stop, or daemon shutdown. `list_processes` is the single process-state snapshot and returns separate `running` and `completed` arrays. Use `read_process_output` only to read captured stdout/stderr by offset, up to 2 MiB per stream per call.

There is no ready queue, completion cursor, long-poll parameter, or per-process status wait API.

`start_process` uses the same managed process registry but keeps stdin available for interactive work.

This separation prevents compilation, tests, and benchmarks from holding a tunnel request open for the entire child-process lifetime.

## ChatGPT app

Keep Rivet running, then open ChatGPT Plugins on the web, select **Add custom MCP server**, and choose **Tunnel** under Connection. Select the tunnel or enter its ID, configure authentication, review the confirmation, and create the plugin. Install it and select it with `@` in the conversation. Call `list_roots` to verify the connection. Workspace access restrictions still apply. See the [official ChatGPT connection steps](https://developers.openai.com/api/docs/guides/custom-mcp-server#how-to-use).

Rivet itself does not create the OpenAI tunnel, create runtime API keys, modify the ChatGPT workspace, or register the app.

## Security

The direct transport:

- uses outbound HTTPS only;
- verifies TLS certificates;
- disables control-plane redirects;
- sends the runtime key only in the Bearer authorization header;
- treats `request_id`, `channel`, and `shard_token` as opaque correlation data;
- keeps MCP file-tool access fixed to the current-directory workspace;
- grants linked-worktree Git metadata access to commands launched as `git` and their descendants;
- runs native-session child commands under Seatbelt on macOS or bubblewrap on Linux/WSL2;
- denies child network access by default; `--network` enables child networking without weakening the filesystem sandbox;
- fails closed when the required sandbox backend is unavailable;
- does not advertise optional tunnel capabilities it does not implement.

## Common errors

- **Authentication or authorization failure:** verify the runtime API key and tunnel permissions.
- **Invalid tunnel ID:** use the tunnel ID created in OpenAI Platform.
- **`STARTUP_ERROR`:** validate the current workspace, registered executables, and requested `--pass-env` variables.
- **Tunnel missing in ChatGPT:** check its target workspace association and your **Tunnels Read + Use** permission.
- **`SANDBOX_ERROR`:** macOS requires the built-in Seatbelt backend; Linux/WSL2 requires `bubblewrap`. On Ubuntu 24.04, install `apparmor-profiles`, copy `bwrap-userns-restrict` from `/usr/share/apparmor/extra-profiles/` to `/etc/apparmor.d/`, and load it with `apparmor_parser -r`.
- **`COMMAND_NOT_FOUND`:** restart the session with that command explicitly allowed.
- **`PATH_DENIED`:** use an absolute path under one of the roots returned by `list_roots`.
- **Long command stops early:** set `timeout_ms` high enough for that command; without it, `run_command` uses `default_timeout_ms`.
