#!/usr/bin/env bash
set -euo pipefail

install_dir="$1"
client="$install_dir/tunnel-client"
latest_url="https://github.com/openai/tunnel-client/releases/latest"

case "$(uname -s)" in
  Darwin) goos=darwin ;;
  Linux) goos=linux ;;
  *) printf 'Unsupported operating system for tunnel-client.\n' >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) goarch=arm64 ;;
  x86_64|amd64) goarch=amd64 ;;
  *) printf 'Unsupported CPU architecture for tunnel-client.\n' >&2; exit 1 ;;
esac

if ! command -v curl >/dev/null || ! command -v unzip >/dev/null; then
  if [[ -x "$client" && -x "$install_dir/cloudflared" ]]; then
    printf 'Cannot check tunnel-client updates: curl and unzip are required; using the installed binary.\n' >&2
    exit 0
  fi
  printf 'Install curl and unzip to download tunnel-client from GitHub.\n' >&2
  exit 1
fi

if ! resolved_url="$(curl -fsSL --connect-timeout 5 --max-time 20 -o /dev/null -w '%{url_effective}' "$latest_url")"; then
  if [[ -x "$client" && -x "$install_dir/cloudflared" ]]; then
    printf 'Could not check the latest tunnel-client release; using the installed binary.\n' >&2
    exit 0
  fi
  printf 'Could not reach GitHub to download tunnel-client.\n' >&2
  exit 1
fi

tag="${resolved_url##*/}"
if [[ ! "$resolved_url" =~ ^https://github\.com/openai/tunnel-client/releases/tag/v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  printf 'Unexpected GitHub latest-release URL: %s\n' "$resolved_url" >&2
  exit 1
fi
version="${tag#v}"
if [[ -x "$client" && -x "$install_dir/cloudflared" ]] && "$client" --version 2>/dev/null | grep -Eq "^${version}(\+|[[:space:]]|$)"; then
  printf 'tunnel-client %s is already current.\n' "$tag"
  exit 0
fi

archive="tunnel-client-${tag}-${goos}-${goarch}.zip"
release_url="https://github.com/openai/tunnel-client/releases/download/${tag}"
mkdir -p "$install_dir"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
curl -fsSL --connect-timeout 5 --max-time 120 "$release_url/SHA256SUMS.txt" -o "$tmp_dir/SHA256SUMS.txt"
expected_hash="$(awk -v name="$archive" '$2 == name || $2 == "*" name {print $1}' "$tmp_dir/SHA256SUMS.txt")"
if [[ ! "$expected_hash" =~ ^[[:xdigit:]]{64}$ ]]; then
  printf 'Official checksum is missing or invalid for %s.\n' "$archive" >&2
  exit 1
fi
curl -fsSL --connect-timeout 5 --max-time 300 "$release_url/$archive" -o "$tmp_dir/$archive"
if command -v sha256sum >/dev/null; then
  actual_hash="$(sha256sum "$tmp_dir/$archive" | awk '{print $1}')"
else
  actual_hash="$(shasum -a 256 "$tmp_dir/$archive" | awk '{print $1}')"
fi
actual_hash="$(printf '%s' "$actual_hash" | tr '[:upper:]' '[:lower:]')"
expected_hash="$(printf '%s' "$expected_hash" | tr '[:upper:]' '[:lower:]')"
if [[ "$actual_hash" != "$expected_hash" ]]; then
  printf 'SHA-256 verification failed for %s.\n' "$archive" >&2
  exit 1
fi

mkdir "$tmp_dir/unpacked"
unzip -q "$tmp_dir/$archive" -d "$tmp_dir/unpacked"
if [[ ! -f "$tmp_dir/unpacked/tunnel-client" || ! -f "$tmp_dir/unpacked/cloudflared" ]]; then
  printf 'The official archive is missing tunnel-client or cloudflared.\n' >&2
  exit 1
fi
chmod +x "$tmp_dir/unpacked/tunnel-client" "$tmp_dir/unpacked/cloudflared"
for file in "$tmp_dir/unpacked"/*; do
  mv -f "$file" "$install_dir/"
done
printf 'Installed official tunnel-client %s for %s-%s (SHA-256 verified).\n' "$tag" "$goos" "$goarch"
