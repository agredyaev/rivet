#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

repo='agredyaev/rivet'
latest_url="https://github.com/$repo/releases/latest"
download_base="https://github.com/$repo/releases/download"
install_root="${RIVET_INSTALL_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/rivet}"
if [[ -t 1 && ${NO_COLOR+x} != x ]]; then
  cyan=$'\033[36m'; green=$'\033[32m'; yellow=$'\033[33m'; red=$'\033[31m'; bold=$'\033[1m'; reset=$'\033[0m'
else
  cyan=''; green=''; yellow=''; red=''; bold=''; reset=''
fi
rivet_header() { printf '\n%bRivet setup%b\n' "$bold$cyan" "$reset"; }
rivet_ok() { printf '  %b✓%b %s\n' "$green" "$reset" "$1"; }
rivet_note() { printf '  %b%s%b\n' "$yellow" "$1" "$reset"; }
rivet_error() { printf '  %b✗%b %s\n' "$red" "$reset" "$1" >&2; }
rivet_header

if [[ "${1:-}" == '--help' || "${1:-}" == '-h' ]]; then
  printf 'Usage: curl -fsSL https://raw.githubusercontent.com/%s/main/scripts/install.sh | bash -s -- [--root PATH] [--allow-command NAME=EXECUTABLE] [--allow-subcommand NAME=VALUE] [--allow-any-args NAME]\n' "$repo"
  printf 'Downloads the latest verified Rivet release, installs it under %s, and starts one tunnel session.\n' "$install_root"
  exit 0
fi

if [[ ! -t 1 || ! -r /dev/tty ]]; then
  printf 'Rivet setup needs an interactive terminal. Run this command in Terminal or PowerShell.\n' >&2
  exit 1
fi
for tool in curl unzip awk; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'Required tool not found: %s\n' "$tool" >&2
    exit 1
  fi
done

case "$(uname -s)" in
  Darwin) os='darwin' ;;
  Linux) os='linux' ;;
  *) printf 'Unsupported operating system: %s\n' "$(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch='x64' ;;
  arm64|aarch64)
    if [[ "$os" == 'darwin' ]]; then arch='arm64'; else arch='aarch64'; fi
    ;;
  *) printf 'Unsupported CPU architecture: %s\n' "$(uname -m)" >&2; exit 1 ;;
esac
if [[ "$os" == 'darwin' && "$arch" != 'arm64' ]]; then
  printf 'Rivet releases currently support macOS arm64 only.\n' >&2
  exit 1
fi
asset="rivet-$os-$arch.zip"

rivet_note 'Finding the latest Rivet release...'
latest_location="$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$latest_url")"
tag="${latest_location##*/}"
if [[ -z "$tag" || "$tag" == 'latest' || "$tag" == *'/'* ]]; then
  rivet_error 'Could not determine the latest release tag.'
  exit 1
fi
rivet_ok "Release: $tag ($asset)"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/rivet-install.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
release_url="$download_base/$tag"
curl -fsSL "$release_url/$asset" -o "$tmp_dir/$asset"
curl -fsSL "$release_url/SHA256SUMS" -o "$tmp_dir/SHA256SUMS"
expected="$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1; exit }' "$tmp_dir/SHA256SUMS")"
if [[ ! "$expected" =~ ^[[:xdigit:]]{64}$ ]]; then
  rivet_error "Release checksum is missing for $asset."
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp_dir/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp_dir/$asset" | awk '{print $1}')"
fi
actual="$(printf '%s' "$actual" | tr '[:upper:]' '[:lower:]')"
expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
if [[ "$actual" != "$expected" ]]; then
  rivet_error 'Release archive checksum does not match.'
  exit 1
fi
rivet_ok 'Release archive checksum verified'

version_dir="$install_root/$tag-${actual:0:12}"
mkdir -p "$version_dir"
unzip -oq "$tmp_dir/$asset" -d "$version_dir"
if [[ ! -x "$version_dir/bin/rivet" || ! -f "$version_dir/rivet.toml" ]]; then
  rivet_error 'Release archive is missing the Rivet binary or configuration.'
  exit 1
fi
rivet_ok "Installed in $version_dir"
rivet_note 'Starting Rivet with the scope for this session...'
rm -rf "$tmp_dir"
trap - EXIT
exec "$version_dir/bin/rivet" session "$@" < /dev/tty
