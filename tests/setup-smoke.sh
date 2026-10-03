#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
package="$fixture/package"
workspace="$fixture/work space"
mkdir -p "$package/bin" "$package/rivet/scripts" "$workspace"
expected_workspace="$(cd -- "$workspace" && pwd -P)"
cp "$repo_dir/start-rivet.sh" "$package/start-rivet.sh"
cp "$repo_dir/scripts/ui.sh" "$package/rivet/scripts/ui.sh"
printf '#!/bin/sh\nexit 0\n' > "$package/bin/rivet"
chmod +x "$package/bin/rivet" "$package/start-rivet.sh"
touch "$package/rivet.toml"
cat > "$package/rivet/scripts/start-tunnel.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$RIVET_PROJECT_DIR/received-args.txt"
EOF
chmod +x "$package/rivet/scripts/start-tunnel.sh"

"$package/start-rivet.sh" \
  --root "$workspace" \
  --allow-command git=git \
  --allow-subcommand git=status
printf '%s\n' \
  --allow-command git=git \
  --allow-subcommand git=status \
  --root "$expected_workspace" > "$fixture/expected-args.txt"
diff -u "$fixture/expected-args.txt" "$package/received-args.txt"
[[ ! -e "$package/rivet-root.txt" ]]
