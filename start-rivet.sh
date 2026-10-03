#!/usr/bin/env bash
set -euo pipefail
set +x

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
rivet="$project_dir/bin/rivet"
if [[ ! -x "$rivet" || ! -f "$project_dir/rivet.toml" ]]; then
  printf 'Extract the Rivet release ZIP and run start-rivet.sh from its folder.\n' >&2
  exit 1
fi
export RIVET_PROJECT_DIR="$project_dir"
exec "$rivet" session "$@"
