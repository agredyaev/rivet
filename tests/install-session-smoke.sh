#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
fake_bin="$fixture/bin"
workspace="$fixture/work space"
mkdir -p "$fake_bin" "$workspace"
cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
cat <<'INSTALLER'
printf '%s\n' "$@" > "$RIVET_SETUP_ARGS_FILE"
INSTALLER
EOF
chmod +x "$fake_bin/curl"

expected_root="$(cd -- "$workspace" && pwd -P)"
export PATH="$fake_bin:$PATH"
export RIVET_SETUP_ARGS_FILE="$fixture/received-args.txt"
setup_output="$(cd "$workspace" && bash "$repo_dir/install-session.sh")"
printf '%s\n' "$setup_output"
[[ "$setup_output" == *"Workspace: $expected_root"* ]]
[[ "$setup_output" == *'Commands added for this session:'* ]]
for expected in 'git status' 'git diff' 'git log' 'git show' 'git add' 'git commit' \
  'cargo (any arguments)' 'uv (any arguments)' 'make (any arguments)'; do
  [[ "$setup_output" == *"✓ $expected"* ]]
done
printf '%s\n' \
  --root "$expected_root" \
  --allow-command git=git \
  --allow-command cargo=cargo \
  --allow-command uv=uv \
  --allow-command make=make \
  --allow-subcommand git=status \
  --allow-subcommand git=diff \
  --allow-subcommand git=log \
  --allow-subcommand git=show \
  --allow-subcommand git=add \
  --allow-subcommand git=commit \
  --allow-any-args cargo \
  --allow-any-args uv \
  --allow-any-args make > "$fixture/expected-args.txt"
diff -u "$fixture/expected-args.txt" "$RIVET_SETUP_ARGS_FILE"
