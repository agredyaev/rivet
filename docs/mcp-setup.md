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

The channel accepts self-contained stateless MCP requests. Multiple ChatGPT conversations can use the same Rivet daemon concurrently; Rivet does not keep per-chat state.

## Start a session

From the workspace directory, run:

```sh
rivet session
```

Rivet shows the effective workspace scope and command permissions. The built-in selector offers `uv`, `mkdir`, `rg`, `make`, restricted `git`, and restricted `cargo`; custom commands can be added explicitly.

Rivet then asks for:

- the OpenAI tunnel ID;
- the tunnel runtime API key.

Create these in OpenAI Platform before starting the session. The runtime API key is not written to Rivet configuration and is not passed on the command line.

For non-interactive operation, provide:

```sh
CONTROL_PLANE_TUNNEL_ID=tunnel_... \
CONTROL_PLANE_API_KEY=... \
rivet session ...
```

Production uses `https://api.openai.com`. A different `CONTROL_PLANE_BASE_URL` is accepted only when `RIVET_ALLOW_UNSAFE_CONTROL_PLANE=1` is set explicitly for testing.

## Session lifecycle

`rivet session`:

1. canonicalizes the current directory as the single workspace root;
2. collects or accepts the command allowlist;
3. builds resource and environment policy from embedded defaults;
4. validates the workspace and mandatory OS sandbox;
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
{"state":"submitted","process_id":123}
```

The child continues independently of the originating tunnel request. Read state and bounded output with:

- `read_process_output`;
- `list_processes`;
- `list_ready_processes`.

`start_process` uses the same managed process registry but keeps stdin available for interactive work.

This separation prevents compilation, tests, benchmarks, and similar long-running work from holding a tunnel request open for the entire child-process lifetime.

## ChatGPT app

In ChatGPT, create or configure an app using **Tunnel**, select the tunnel ID, scan tools, and enable the app in the chat. Call `list_roots` to verify the connection.

Rivet itself does not create the OpenAI tunnel, create runtime API keys, modify the ChatGPT workspace, or register the app.

## Security

The direct transport:

- uses outbound HTTPS only;
- verifies TLS certificates;
- disables control-plane redirects;
- sends the runtime key only in the Bearer authorization header;
- treats `request_id`, `channel`, and `shard_token` as opaque correlation data;
- never broadens the current-directory workspace or command permissions;
- runs native-session child commands under Seatbelt on macOS or bubblewrap on Linux/WSL2;
- denies child network access by default; `--network` enables child networking without weakening the filesystem sandbox;
- fails closed when the required sandbox backend is unavailable;
- does not advertise optional tunnel capabilities it does not implement.

## Common errors

- **Authentication or authorization failure:** verify the runtime API key and tunnel permissions.
- **Invalid tunnel ID:** use the tunnel ID created in OpenAI Platform.
- **`CONFIG_ERROR`:** validate the current workspace and allowed executables.
- **`SANDBOX_ERROR`:** macOS requires the built-in Seatbelt backend; Linux/WSL2 requires the `bubblewrap` package.
- **`COMMAND_NOT_FOUND`:** restart the session with that command explicitly allowed.
- **`PATH_DENIED`:** use an absolute path under one of the roots returned by `list_roots`.
- **Long command stops early:** set an appropriate `timeout_ms`; `run_command` otherwise uses `default_timeout_ms`.
