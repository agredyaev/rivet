# Connect Rivet to ChatGPT

Rivet uses MCP stdio. ChatGPT connects to remote MCP servers; to reach this local server, use OpenAI Secure MCP Tunnel. Before connecting, install Rivet and `rivet.toml`, then run `rivet config-check` and `rivet doctor` successfully.

## ChatGPT

ChatGPT cannot launch a local stdio process. Connect it through [OpenAI Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels). Rivet exposes command execution and file writes, so the ChatGPT plan and workspace must allow full MCP tool access. The [ChatGPT MCP help page](https://help.openai.com/en/articles/12584461-developer-mode-and-mcp-apps-in-chatgpt) lists current availability.

1. Create a tunnel in [OpenAI Platform tunnel settings](https://platform.openai.com/settings/organization/tunnels). Associate it with the target ChatGPT workspace. Creating a tunnel requires Tunnels Read + Manage; using it requires Tunnels Read + Use.
2. Create a runtime API key in [OpenAI Platform API key settings](https://platform.openai.com/settings/organization/api-keys). Do not put the key in `rivet.toml`, a profile file, or shell history.
3. Install the current platform archive from [OpenAI's tunnel-client releases](https://github.com/openai/tunnel-client/releases). Keep `tunnel-client` and the bundled `cloudflared` executable together in `bin/`.
4. From the workspace root, run the script for your shell. The default layout is `bin/` for the executables, `rivet.toml` for config, and `rivet/scripts/` for these scripts. Set `RIVET_PROJECT_DIR` if your layout differs. Each script prompts for the tunnel ID and reads the runtime key without displaying it. The generated profile stores only the `env:CONTROL_PLANE_API_KEY` reference. A small launcher removes the key from its environment before starting Rivet. The script validates Rivet, runs tunnel-client `doctor`, then starts the tunnel in the foreground:

   Bash on macOS or Linux:

   ```sh
   ./rivet/scripts/start-tunnel.sh
   ```

   PowerShell on Windows:

   ```powershell
   .\rivet\scripts\start-tunnel.ps1
   ```

   Keep that process running. In ChatGPT, open **Settings → Apps → Create**. If app creation or Developer mode is unavailable, ask the workspace admin to enable it. Enter the app name and description, choose **Tunnel**, select the tunnel ID, then select **Scan Tools** and **Create**. Enable the app in a chat and call `list_roots` to verify the connection.

### App metadata

Set the user-facing app name and description in the ChatGPT app creation form. The MCP `initialize` response supplies Rivet's server identity: name `rivet`, title `Rivet`, version from `Cargo.toml`, a short description, and the project URL. Scan or refresh the app's tools after changing server metadata.

The `Developer` and `Category` values belong to ChatGPT's app or listing metadata; the MCP server cannot set them. The version shown by ChatGPT for the app is also separate from Rivet's MCP server version. This repository has no plugin package manifest. Public plugin packages use `plugin.json` for listing metadata such as `developerName`, `category`, and `websiteURL`; that manifest is not used to configure a private app connected through Secure MCP Tunnel.

The tunnel process must remain connected. If Rivet or the host stops, ChatGPT cannot call its tools. See OpenAI's [Secure MCP Tunnel setup guide](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) for current permission and network requirements.

## Common connection errors

- **MCP client reports startup failure:** run `rivet config-check --config /absolute/path/rivet.toml` and `rivet doctor --config /absolute/path/rivet.toml`. Fix the reported error, then restart the MCP client. `rivet serve` waits for requests on stdin; run it through the client after validation.
- **`CONFIG_ERROR`:** read the error printed to stderr. Set `allowed_roots` to an existing absolute directory, remove unknown TOML fields, and make sure every configured executable exists.
- **`COMMAND_NOT_FOUND`:** the requested command name is absent from `[commands.*]`. Add it to `rivet.toml`, then restart Rivet.
- **Executable missing at startup:** run `rivet config-check --config /absolute/path/rivet.toml`. Fix the reported path, or set `[commands.NAME].executable` to the executable's absolute path.
- **`PATH_DENIED`:** set `cwd` or the file path to an absolute path under one of the effective roots. In MCP, call `list_roots` to see those roots.
- **ChatGPT cannot see the tunnel:** confirm the tunnel is associated with the ChatGPT workspace and that the user has Tunnels Read + Use permission.
