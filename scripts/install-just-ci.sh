#!/usr/bin/env bash
# Install the pinned just binary used by GitHub Actions after verifying its release checksum.
set -euo pipefail

version=1.56.0
platform=''
archive=''
sha256=''
uname_s="$(uname -s)"
uname_m="$(uname -m)"
windows_host=false

case "$uname_s:$uname_m" in
  Linux:x86_64|Linux:amd64)
    platform=x86_64-unknown-linux-musl
    sha256=fa2a8ec1015d9df5330941ade12437488fc40d33f9c9f8cd4eb70a26de11b639
    ;;
  Linux:aarch64|Linux:arm64)
    platform=aarch64-unknown-linux-musl
    sha256=c8c1d656e9f47569ec1ae2bf8779af2621cdeea6bbbba3b0cacd64f951d25e2b
    ;;
  Darwin:x86_64|Darwin:amd64)
    platform=x86_64-apple-darwin
    sha256=09b35ff6d17023ffae37ce408d1a78a976d9e001cae54b88e238f7f40db9b783
    ;;
  Darwin:arm64|Darwin:aarch64)
    platform=aarch64-apple-darwin
    sha256=f35798d4bcdc4db020eef7d2853ad98bbfb97a4d29ee695ba042f18e7fedcc11
    ;;
  MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
    platform=x86_64-pc-windows-msvc
    sha256=804f5b2fe94291d0df38fd8dfc5620afbf3496f8f11c1915b42a87323234f0ba
    windows_host=true
    ;;
  *)
    echo "error: no pinned just build for $uname_s/$uname_m" >&2
    exit 1
    ;;
esac

case "$platform" in
  *-windows-*) archive="just-${version}-${platform}.zip" ;;
  *) archive="just-${version}-${platform}.tar.gz" ;;
esac

base_url="https://github.com/casey/just/releases/download/${version}"
work_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
if [[ "$windows_host" == true && "$work_root" == *:* ]]; then
  work_root="$(cygpath -u "$work_root")"
fi
tmp="$(mktemp -d "$work_root/spurfire-just.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

curl --fail --location --retry 3 --proto '=https' --tlsv1.2 \
  --output "$tmp/$archive" "$base_url/$archive"

if command -v sha256sum >/dev/null 2>&1; then
  printf '%s  %s\n' "$sha256" "$tmp/$archive" | sha256sum -c -
else
  actual="$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')"
  [[ "$actual" == "$sha256" ]] || {
    echo "error: just archive checksum mismatch" >&2
    exit 1
  }
fi

install_dir="$work_root/just-${version}-${platform}"
rm -rf "$install_dir"
mkdir -p "$install_dir"
case "$archive" in
  *.zip) unzip -q "$tmp/$archive" -d "$install_dir" ;;
  *.tar.gz) tar -xzf "$tmp/$archive" -C "$install_dir" ;;
esac

just_bin="$install_dir/just"
[[ "$platform" == *-windows-* ]] && just_bin="$install_dir/just.exe"
[[ -x "$just_bin" || -f "$just_bin" ]] || {
  echo "error: archive did not contain the expected just executable" >&2
  exit 1
}
chmod +x "$just_bin" 2>/dev/null || true
"$just_bin" --version | grep -Fx "just $version"

if [[ -n "${GITHUB_PATH:-}" ]]; then
  github_path_file="$GITHUB_PATH"
  path_entry="$install_dir"
  if [[ "$windows_host" == true ]]; then
    github_path_file="$(cygpath -u "$github_path_file")"
    path_entry="$(cygpath -w "$path_entry")"
  fi
  printf '%s\n' "$path_entry" >> "$github_path_file"
else
  printf 'installed just at %s; add %s to PATH\n' "$just_bin" "$install_dir"
fi
