#!/usr/bin/env bash
set -euo pipefail

repo="${NYAN_REPO:-blxzer77/nyan}"
install_dir="${NYAN_INSTALL_DIR:-$HOME/.local/bin}"
requested_version="${1:-${NYAN_VERSION:-}}"

if [ "$#" -gt 1 ]; then
  printf 'usage: install.sh [version or tag]\n' >&2
  exit 2
fi

case "$(uname -s)" in
  Linux) ;;
  *)
    printf 'install.sh only supports Linux; use install.ps1 on Windows.\n' >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64|amd64) asset_arch=x86_64 ;;
  aarch64|arm64) asset_arch=aarch64 ;;
  *)
    printf 'unsupported Linux architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

if [ -z "$requested_version" ]; then
  latest_url="https://github.com/$repo/releases/latest"
  resolved_url="$(curl --fail --silent --show-error --location --output /dev/null --write-out '%{url_effective}' "$latest_url")"
  requested_version="${resolved_url##*/}"
fi

case "$requested_version" in
  nyan-v*) tag="$requested_version" ;;
  *) tag="nyan-v$requested_version" ;;
esac

version="${tag#nyan-v}"
asset="nyan-$tag-linux-$asset_arch.tar.gz"
base_url="https://github.com/$repo/releases/download/$tag"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

archive="$tmp_dir/$asset"
sums="$tmp_dir/SHA256SUMS"
curl --fail --silent --show-error --location --output "$archive" "$base_url/$asset"
curl --fail --silent --show-error --location --output "$sums" "$base_url/SHA256SUMS"

expected="$(awk -v asset="$asset" '$2 == asset { print $1; exit }' "$sums")"
if [[ ! "$expected" =~ ^[[:xdigit:]]{64}$ ]]; then
  printf 'SHA256SUMS has no valid entry for %s\n' "$asset" >&2
  exit 1
fi
printf '%s  %s\n' "$expected" "$archive" | sha256sum --check --status -

mkdir -p "$tmp_dir/unpacked" "$install_dir"
tar -xzf "$archive" -C "$tmp_dir/unpacked"
install -m 0755 "$tmp_dir/unpacked/nyan" "$install_dir/nyan"
printf 'Installed nyan %s to %s/nyan\n' "$version" "$install_dir"
if [[ ":$PATH:" != *":$install_dir:"* ]]; then
  printf 'Add %s to PATH if it is not already present.\n' "$install_dir"
fi
