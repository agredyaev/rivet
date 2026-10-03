# Connect an MCP client

Rivet uses MCP stdio. A local client starts `rivet serve` as a child process; do not enter a URL. Before connecting, install Rivet and `rivet.toml`, then run `rivet config-check` and `rivet doctor` successfully.

Use absolute paths in the client configuration. Replace `/Users/alex/...` with the real file paths on your machine. On Windows, use absolute Windows paths and `rivet.exe`.

## Codex CLI

Run this in a terminal:

```sh
codex mcp add rivet -- "/Users/alex/bin/rivet" serve --config "/Users/alex/.config/rivet/rivet.toml"
codex mcp list
```

On Windows, use the executable's absolute path:

```powershell
codex mcp add rivet -- "C:/Tools/Rivet/rivet.exe" serve --config "C:/Users/alex/AppData/Roaming/Rivet/rivet.toml"
codex mcp list
```

Start Codex and enter `/mcp`. Select `rivet` and confirm that its tools are listed. Codex stores this server in its MCP configuration. Remove it with `codex mcp remove rivet`.

This configures a local stdio server in Codex. The first path is the executable; the remaining words are its arguments. See [Codex MCP setup](https://developers.openai.com/codex/mcp/) for the current CLI and configuration options.

## ChatGPT Desktop

1. Open **Settings → MCP servers**.
2. Select **Add server**.
3. Enter `Rivet` as the name and choose **STDIO**.
4. Set **Command** to `/Users/alex/bin/rivet`.
5. Set **Arguments** to `serve` and `--config /Users/alex/.config/rivet/rivet.toml` as separate arguments:

   ```json
   ["serve", "--config", "/Users/alex/.config/rivet/rivet.toml"]
   ```

6. Save the server and select **Restart**.
7. In a new chat, enter `/mcp` and confirm that `Rivet` is connected.

These steps follow the [ChatGPT MCP client instructions](https://developers.openai.com/codex/mcp/).

On Windows, use the full path to `rivet.exe` as **Command**, for example `C:/Tools/Rivet/rivet.exe`; keep the arguments the same except for the configuration path.

## ChatGPT Web

ChatGPT Web cannot launch a local stdio process. Connect it through [OpenAI Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels). Rivet exposes command execution and file writes, so the ChatGPT plan and workspace must allow full MCP tool access. The [ChatGPT MCP help page](https://help.openai.com/en/articles/12584461-developer-mode-and-mcp-apps-in-chatgpt) lists current availability.

1. Create a tunnel in [OpenAI Platform tunnel settings](https://platform.openai.com/settings/organization/tunnels). Copy its `tunnel_id` and obtain a runtime API key. Creating a tunnel requires Tunnels Read + Manage; running the client and selecting the tunnel require Tunnels Read + Use. Associate the tunnel with the target ChatGPT workspace.
2. Download and unpack the current `tunnel-client` release from [OpenAI's tunnel-client repository](https://github.com/openai/tunnel-client). Add its executable to `PATH`; verify it with `tunnel-client help quickstart`.
3. Keep the runtime key in the environment. Do not put it in `rivet.toml` or commit it.

On macOS or Linux, set the key and create a profile. Replace the sample ID and paths with the values from steps 1–2:

```sh
export CONTROL_PLANE_API_KEY="YOUR_RUNTIME_API_KEY"

tunnel-client init \
  --sample sample_mcp_stdio_local \
  --profile rivet \
  --tunnel-id tunnel_0123456789abcdef0123456789abcdef \
  --mcp-command "/Users/alex/bin/rivet serve --config /Users/alex/.config/rivet/rivet.toml"

tunnel-client doctor --profile rivet --explain
tunnel-client run --profile rivet
```

4. Leave `tunnel-client run` running. In ChatGPT, enable Developer mode in **Settings → Security & login**. If the setting is unavailable, ask the workspace admin to enable it. Open **Plugins**, select **+**, create an app, choose **Tunnel** as the connection, and select the tunnel ID from step 1. Enable the app in a chat and call `list_roots` to verify the connection.

For ChatGPT Web access, the tunnel process must remain connected. If Rivet or the host stops, ChatGPT cannot call its tools. See OpenAI's [Secure MCP Tunnel setup guide](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) for tunnel creation, workspace association, and current access requirements.

## Common connection errors

- **MCP client reports startup failure:** run `rivet config-check --config /absolute/path/rivet.toml` and `rivet doctor --config /absolute/path/rivet.toml`. Fix the reported error, then restart the MCP client. `rivet serve` waits for requests on stdin; run it through the client after validation.
- **`CONFIG_ERROR`:** read the error printed to stderr. Set `allowed_roots` to an existing absolute directory, remove unknown TOML fields, and make sure every configured executable exists.
- **`COMMAND_NOT_FOUND`:** the requested command name is absent from `[commands.*]`. Add it to `rivet.toml`, then restart Rivet.
- **Executable missing at startup:** run `rivet config-check --config /absolute/path/rivet.toml`. Fix the reported path, or set `[commands.NAME].executable` to the executable's absolute path.
- **`PATH_DENIED`:** set `cwd` or the file path to an absolute path under one of the effective roots. In MCP, call `list_roots` to see those roots.
- **ChatGPT cannot see the tunnel:** confirm the tunnel is associated with the ChatGPT workspace and that the user has Tunnels Read + Use permission.
