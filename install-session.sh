#!/usr/bin/env bash
set -euo pipefail
set +x

workspace_root="$(pwd -P)"
allowed_commands=('git=git' 'cargo=cargo' 'uv=uv' 'make=make')
allowed_git_subcommands=(status diff log show add commit)
commands_allow_any_args=(cargo uv make)
install_args=(--root "$workspace_root")

if [[ -t 1 && ${NO_COLOR+x} != x ]]; then
  cyan=$'\033[36m'; green=$'\033[32m'; reset=$'\033[0m'
else
  cyan=''; green=''; reset=''
fi
printf '\n%bRivet session scope%b\n' "$cyan" "$reset"
printf '  Workspace: %s\n' "$workspace_root"
printf '  Commands added for this session:\n'
for command in "${allowed_commands[@]}"; do
  install_args+=(--allow-command "$command")
done
for subcommand in "${allowed_git_subcommands[@]}"; do
  printf '    %b✓%b git %s\n' "$green" "$reset" "$subcommand"
  install_args+=(--allow-subcommand "git=$subcommand")
done
for command in "${commands_allow_any_args[@]}"; do
  printf '    %b✓%b %s (any arguments)\n' "$green" "$reset" "$command"
  install_args+=(--allow-any-args "$command")
done
printf '  Scope applies only to this tunnel session.\n\n'

curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/install.sh |
  bash -s -- "${install_args[@]}"
