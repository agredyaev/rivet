#!/usr/bin/env bash
set -euo pipefail
set +x

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="${RIVET_PROJECT_DIR:-$(cd -- "$script_dir/../.." && pwd)}"
rivet="$project_dir/bin/rivet"
config="$project_dir/rivet.toml"
tunnel_client="$project_dir/bin/tunnel-client"
profile_dir="$project_dir/tunnel-client-profiles"
mcp_launcher="$script_dir/serve-rivet-mcp.sh"
tunnel_sync="$script_dir/sync-tunnel-client.sh"
rivet_args=("$@")
ui="$script_dir/ui.sh"
if [[ -f "$ui" ]]; then source "$ui"; fi

for path in "$rivet" "$config" "$mcp_launcher" "$tunnel_sync"; do
  if [[ ! -f "$path" ]]; then
    printf 'Required file not found: %s\n' "$path" >&2
    exit 1
  fi
done
if [[ ! -x "$rivet" || ! -x "$mcp_launcher" || ! -x "$tunnel_sync" ]]; then
  printf 'Rivet, the MCP launcher, and the tunnel installer must be executable.\n' >&2
  exit 1
fi

rivet_note 'Checking Rivet configuration and workspace access...'
if ! check_output="$("$rivet" config-check --config "$config" "${rivet_args[@]}" 2>&1)"; then
  printf '%s\n' "$check_output" >&2
  rivet_error 'Configuration check failed.'
  exit 1
fi
rivet_ok 'Configuration is valid'
if ! check_output="$("$rivet" doctor --config "$config" "${rivet_args[@]}" 2>&1)"; then
  printf '%s\n' "$check_output" >&2
  rivet_error 'Rivet doctor failed.'
  exit 1
fi
rivet_ok 'Workspace access is ready'

rivet_note 'Preparing the official OpenAI tunnel client...'
"$tunnel_sync" "$project_dir/bin"
if [[ ! -x "$tunnel_client" ]]; then
  printf 'tunnel-client was not installed successfully.\n' >&2
  exit 1
fi
rivet_ok 'tunnel-client is ready'

export TUNNEL_CLIENT_PROFILE_DIR="$profile_dir"
printf '\n  OpenAI tunnel: https://platform.openai.com/settings/organization/tunnels\n'
read -r -s -p '  Tunnel ID (input hidden): ' tunnel_id
printf '\n'
if [[ ! "$tunnel_id" =~ ^tunnel_[[:xdigit:]]{32}$ ]]; then
  rivet_error 'Expected tunnel_ followed by 32 hexadecimal characters.'
  exit 1
fi

mkdir -p "$profile_dir"
mcp_command="\"$mcp_launcher\""
for arg in "${rivet_args[@]}"; do
  escaped="${arg//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  mcp_command+=" \"$escaped\""
done
if ! "$tunnel_client" init --force \
  --sample sample_mcp_stdio_local \
  --profile rivet \
  --tunnel-id "$tunnel_id" \
  --mcp-command "$mcp_command" \
  --control-plane-api-key-ref env:CONTROL_PLANE_API_KEY; then
  rivet_error 'Could not create the session tunnel profile.'
  exit 1
fi
unset tunnel_id mcp_command escaped arg

rivet_note 'Checking credentials and starting the tunnel...'
printf '  Runtime API key: https://platform.openai.com/settings/organization/api-keys\n'
read -r -s -p '  Runtime API key (input hidden): ' api_key
printf '\n'
if [[ -z "$api_key" ]]; then
  printf 'Runtime API key cannot be empty.\n' >&2
  exit 1
fi
export CONTROL_PLANE_API_KEY="$api_key"
unset api_key

cleanup_session() {
  unset CONTROL_PLANE_API_KEY
  rm -f "$profile_dir/rivet.yaml"
}
trap cleanup_session EXIT

if ! "$tunnel_client" doctor --profile rivet --explain; then
  rivet_error 'Tunnel credentials or scope validation failed.'
  exit 1
fi
rivet_ok 'Credentials accepted'
rivet_note 'Tunnel is running. Keep this terminal open; press Ctrl+C to stop it.'
"$tunnel_client" run --profile rivet
