#!/usr/bin/env bash
set -euo pipefail
set +x

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="${RIVET_PROJECT_DIR:-$(cd -- "$script_dir/../.." && pwd)}"
rivet="$project_dir/bin/rivet"
config="$project_dir/rivet.toml"
tunnel_client="$project_dir/bin/tunnel-client"
profile_dir="$project_dir/tunnel-client"
mcp_launcher="$script_dir/serve-rivet-mcp.sh"

for path in "$rivet" "$config" "$tunnel_client" "$mcp_launcher"; do
  if [[ ! -f "$path" ]]; then
    printf 'Required file not found: %s\n' "$path" >&2
    exit 1
  fi
done
if [[ ! -x "$rivet" || ! -x "$tunnel_client" || ! -x "$mcp_launcher" ]]; then
  printf 'Rivet, tunnel-client, and the MCP launcher must be executable.\n' >&2
  exit 1
fi

"$rivet" config-check --config "$config"
"$rivet" doctor --config "$config"

read -r -p 'OpenAI tunnel ID: ' tunnel_id
if [[ ! "$tunnel_id" =~ ^tunnel_[[:xdigit:]]{32}$ ]]; then
  printf 'Expected tunnel_ followed by 32 hexadecimal characters.\n' >&2
  exit 1
fi

mkdir -p "$profile_dir"
export TUNNEL_CLIENT_PROFILE_DIR="$profile_dir"
mcp_command="\"$mcp_launcher\""
"$tunnel_client" init \
  --sample sample_mcp_stdio_local \
  --profile rivet \
  --force \
  --tunnel-id "$tunnel_id" \
  --mcp-command "$mcp_command" \
  --control-plane-api-key-ref env:CONTROL_PLANE_API_KEY
unset tunnel_id mcp_command

read -r -s -p 'OpenAI runtime API key (input hidden): ' api_key
printf '\n'
if [[ -z "$api_key" ]]; then
  printf 'Runtime API key cannot be empty.\n' >&2
  exit 1
fi
export CONTROL_PLANE_API_KEY="$api_key"
unset api_key

"$tunnel_client" doctor --profile rivet --explain
exec "$tunnel_client" run --profile rivet
