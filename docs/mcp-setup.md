# Connect Rivet to ChatGPT

Rivet uses MCP stdio. ChatGPT connects to remote MCP servers; to reach this local server, use OpenAI Secure MCP Tunnel. The one-command installer downloads and verifies the latest Rivet release, installs it, asks for this session's scope and credentials, then starts the tunnel.

## ChatGPT

ChatGPT cannot launch a local stdio process. Connect it through [OpenAI Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels). Rivet exposes command execution and file writes, so the ChatGPT plan and workspace must allow full MCP tool access. The [ChatGPT MCP help page](https://help.openai.com/en/articles/12584461-developer-mode-and-mcp-apps-in-chatgpt) lists current availability.

1. From the workspace directory, run the one-command installer and pass the scope for this session:

   macOS or Linux:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.sh | bash -s -- --root "$PWD"
   ```

   Windows PowerShell:

   ```powershell
   & ([scriptblock]::Create((Invoke-RestMethod 'https://raw.githubusercontent.com/agredyaev/rivet/main/scripts/install.ps1'))) -Root (Get-Location).Path
   ```

   The installer detects the platform, downloads the latest release ZIP, verifies its SHA-256 checksum, and installs it under the user's local application data directory. Rivet then presents the workspace and command selection in its CLI. It offers `uv`, `mkdir`, `rg`, and `make` with any arguments. For `git`, allowed first arguments are `status`, `diff`, `log`, `show`, `add`, `commit`, `rev-parse`, and `ls-files`; for `cargo`, they are `check`, `test`, `fmt`, `clippy`, and `metadata`. Remaining arguments pass directly to the executable. Enter multiple menu numbers separated by commas to select multiple built-ins. Select **Add a custom command** to add one or more commands, then set their allowed first arguments or choose any arguments. Review the resulting scope before starting the tunnel.

   Rivet checks the latest stable release in the official [OpenAI tunnel-client repository](https://github.com/openai/tunnel-client/releases), downloads the matching platform archive if needed, verifies the SHA-256 from that same release, and installs `tunnel-client` with its bundled `cloudflared`. If GitHub is unavailable, an installed client is reused; the first install requires internet access.

   Rivet asks for the OpenAI tunnel ID and runtime API key. Create them in [Platform Tunnels](https://platform.openai.com/settings/organization/tunnels) and [Runtime API keys](https://platform.openai.com/settings/organization/api-keys); the links are printed in the terminal. Creating a tunnel requires Tunnels Read + Manage; using it requires Tunnels Read + Use. Both values are entered with hidden input. The runtime key is not saved. The selected roots and allowed programs apply only to this server process. Keep the terminal open while connected.

   In ChatGPT, open **Settings → Apps → Create**. If app creation or Developer mode is unavailable, ask the workspace admin to enable it. Enter the app name and description, choose **Tunnel**, select the tunnel ID, then select **Scan Tools** and **Create**. Enable the app in a chat and call `list_roots` to verify the connection.

## Session lifecycle

Release packages contain `bin/rivet` and `rivet.toml`. The Bash and PowerShell installers are in `scripts/` in the source repository; the package runs setup through `rivet session` without helper scripts. The TOML file stores resource limits and environment policy. Workspace roots and command permissions are collected for each session and passed as startup arguments, not saved in TOML.

`rivet session` performs these steps:

1. Collects the workspace roots, selected built-in commands, and custom commands, then displays the effective scope.
2. Validates the configuration, roots, and executables with `rivet config-check` and `rivet doctor`.
3. Checks the latest release in the official [OpenAI tunnel-client repository](https://github.com/openai/tunnel-client/releases), selects the full client archive for the detected OS and architecture, downloads it with `curl`, and verifies its SHA-256 using that release's `SHA256SUMS.txt`. It extracts `tunnel-client` and the bundled `cloudflared` into the package's `bin` directory.
4. Requests and validates the tunnel ID, then creates a temporary tunnel profile with the current session scope.
5. Requests the runtime API key with hidden input, runs `tunnel-client doctor`, then starts `tunnel-client run` in the foreground. Keep the terminal open; press Ctrl+C to stop it. Rivet removes the temporary profile when the tunnel process exits.

The API key is not written to the profile or passed on a command line. It is supplied to tunnel-client through `CONTROL_PLANE_API_KEY`. The tunnel starts Rivet through an internal subcommand that removes `CONTROL_PLANE_API_KEY`, `OPENAI_API_KEY`, and `OPENAI_ADMIN_KEY` from the MCP server process environment.

Rivet does not create a tunnel or API key in OpenAI Platform, change the ChatGPT workspace, or register Codex. It prints the Platform links, then accepts the tunnel ID and runtime key. The API key is requested each time. The tunnel profile is recreated with the current scope for each launch and removed when the session ends. Do not paste the API key into a command, config file, or profile.

The interactive selector starts with no commands selected. If you select none and add none, command/process launch requests return `COMMAND_NOT_FOUND`. Rivet starts allowed executables directly without a shell or interactive terminal. See [session scope arguments](configuration.md#allowed-programs).

### App metadata

Set the user-facing app name and description in the ChatGPT app creation form. The MCP `initialize` response supplies Rivet's server identity: name `rivet`, title `Rivet`, version from `Cargo.toml`, a short description, and the project URL. Scan or refresh the app's tools after changing server metadata.

The `Developer` and `Category` values belong to ChatGPT's app or listing metadata; the MCP server cannot set them. The version shown by ChatGPT for the app is also separate from Rivet's MCP server version. This repository has no plugin package manifest. Public plugin packages use `plugin.json` for listing metadata such as `developerName`, `category`, and `websiteURL`; that manifest is not used to configure a private app connected through Secure MCP Tunnel.

The tunnel process must remain connected. If Rivet or the host stops, ChatGPT cannot call its tools. See OpenAI's [Secure MCP Tunnel setup guide](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) for current permission and network requirements.

## Common connection errors

- **MCP client reports startup failure:** run `rivet config-check --config /absolute/path/rivet.toml --root /absolute/workspace` and `rivet doctor --config /absolute/path/rivet.toml --root /absolute/workspace`. Fix the reported error, then restart the MCP client. `rivet serve` waits for requests on stdin; run it through the client after validation.
- **`CONFIG_ERROR`:** read the error printed to stderr. Check each `--root` path exists and each `--allow-command NAME=EXECUTABLE` resolves to an executable.
- **`COMMAND_NOT_FOUND`:** the requested command name is absent from the session's `--allow-command` arguments. Restart with that program explicitly allowed.
- **Executable missing at startup:** run `rivet config-check` with the same roots and allowlist arguments. Fix the executable name or pass an absolute executable path.
- **`PATH_DENIED`:** set `cwd` or the file path to an absolute path under one of the effective roots. In MCP, call `list_roots` to see those roots.
- **ChatGPT cannot see the tunnel:** confirm the tunnel is associated with the ChatGPT workspace and that the user has Tunnels Read + Use permission.
