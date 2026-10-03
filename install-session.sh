#!/usr/bin/env bash
set -euo pipefail
set +x

workspace_root="$(pwd -P)"
allowed_commands=('git=git' 'cargo=cargo' 'uv=uv' 'make=make')
allowed_git_subcommands=(status diff log show add commit)
commands_allow_any_args=(cargo uv make)
install_args=(--root "$workspace_root")

for command in "${allowed_commands[@]}"; do
  install_args+=(--allow-command "$command")
done
for subcommand in "${allowed_git_subcommands[@]}"; do
  install_args+=(--allow-subcommand "git=$subcommand")
done
for command in "${commands_allow_any_args[@]}"; do
  install_args+=(--allow-any-args "$command")
done

curl -fsSL https://raw.githubusercontent.com/agredyaev/rivet/main/install.sh |
  bash -s -- "${install_args[@]}"
