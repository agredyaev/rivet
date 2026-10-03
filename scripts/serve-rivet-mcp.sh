#!/usr/bin/env bash
set -eu
set +x

unset CONTROL_PLANE_API_KEY OPENAI_API_KEY OPENAI_ADMIN_KEY
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="${RIVET_PROJECT_DIR:-$(cd -- "$script_dir/../.." && pwd)}"
exec "$project_dir/bin/rivet" serve --config "$project_dir/rivet.toml"
