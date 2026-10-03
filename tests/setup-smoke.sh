#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
package="$fixture/package"
mkdir -p "$package/bin"
cp "$repo_dir/start-rivet.sh" "$package/start-rivet.sh"
chmod +x "$package/start-rivet.sh"
touch "$package/rivet.toml"
cat > "$package/bin/rivet" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$RIVET_PROJECT_DIR/received-args.txt"
EOF
chmod +x "$package/bin/rivet"

"$package/start-rivet.sh" --root "$fixture" --allow-command mycmd=my-exe
printf '%s\n' session --root "$fixture" --allow-command mycmd=my-exe > "$fixture/expected-args.txt"
diff -u "$fixture/expected-args.txt" "$package/received-args.txt"
