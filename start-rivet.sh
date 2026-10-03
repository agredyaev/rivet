#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
rivet="$project_dir/bin/rivet"
config="$project_dir/rivet.toml"
ui="$project_dir/rivet/scripts/ui.sh"
if [[ ! -f "$ui" ]]; then ui="$project_dir/scripts/ui.sh"; fi
if [[ -f "$ui" ]]; then source "$ui"; fi
rivet_args=()
roots=()

if [[ ! -x "$rivet" || ! -f "$config" ]]; then
  printf 'This is not a complete Rivet release package. Extract the platform ZIP and run start-rivet.sh from its folder.\n' >&2
  exit 1
fi

while (($#)); do
  case "$1" in
    --root)
      if (($# < 2)); then rivet_error '--root requires a directory'; exit 1; fi
      roots+=("$2")
      shift 2
      ;;
    --allow-command|--allow-subcommand|--allow-any-args)
      if (($# < 2)); then rivet_error "$1 requires a value"; exit 1; fi
      rivet_args+=("$1" "$2")
      shift 2
      ;;
    --help|-h)
      printf 'Usage: ./start-rivet.sh --root PATH [--root PATH ...] [--allow-command NAME=EXECUTABLE]... [--allow-subcommand NAME=VALUE]... [--allow-any-args NAME]...\n'
      exit 0
      ;;
    *) rivet_error "Unknown option: $1"; exit 1 ;;
  esac
done

if [[ "${RIVET_UI_STARTED:-0}" != '1' ]]; then rivet_header; fi
unset RIVET_UI_STARTED
if ((${#roots[@]} == 0)); then
  printf '  Rivet can access only the workspace you choose for this session.\n\n'
  default_root="$(pwd -P)"
  read -r -p "  Workspace directory [$default_root]: " workspace_root
  workspace_root="${workspace_root:-$default_root}"
  roots+=("$workspace_root")
fi

for workspace_root in "${roots[@]}"; do
  if [[ "$workspace_root" == '~' ]]; then
    workspace_root="$HOME"
  elif [[ "$workspace_root" == '~/'* ]]; then
    workspace_root="$HOME/${workspace_root#~/}"
  fi
  if [[ ! -d "$workspace_root" ]]; then rivet_error "Directory not found: $workspace_root"; exit 1; fi
  workspace_root="$(cd -- "$workspace_root" && pwd -P)"
  rivet_args+=(--root "$workspace_root")
  rivet_ok "Workspace: $workspace_root"
done

export RIVET_PROJECT_DIR="$project_dir"
tunnel_launcher="$project_dir/rivet/scripts/start-tunnel.sh"
if [[ ! -f "$tunnel_launcher" ]]; then
  tunnel_launcher="$project_dir/scripts/start-tunnel.sh"
fi
if [[ ! -f "$tunnel_launcher" ]]; then
  printf 'Tunnel launcher is missing from this package.\n' >&2
  exit 1
fi
exec "$tunnel_launcher" "${rivet_args[@]}"
