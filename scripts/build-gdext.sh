#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-gdext.sh [debug|release]
       scripts/build-gdext.sh --self-test

Builds the native spurfire-gdext library and copies it into game/bin/<platform>/.
Environment: CARGO (default: cargo), CARGO_TARGET_DIR (default: <repo>/target).
EOF
}

platform_details() {
  local uname_s uname_m
  uname_s="$(uname -s)"
  uname_m="$(uname -m)"

  case "$uname_s" in
    Darwin) platform="macos"; library="libspurfire_gdext.dylib" ;;
    Linux) platform="linux"; library="libspurfire_gdext.so" ;;
    MINGW*|MSYS*|CYGWIN*) platform="windows"; library="spurfire_gdext.dll" ;;
    *) echo "error: unsupported operating system: $uname_s" >&2; return 1 ;;
  esac

  case "$uname_m" in
    x86_64|amd64|AMD64) arch="x86_64" ;;
    arm64|aarch64) arch="arm64" ;;
    *) echo "error: unsupported architecture: $uname_m" >&2; return 1 ;;
  esac
}

self_test() {
  platform_details
  case "$platform:$library" in
    macos:*.dylib|linux:*.so|windows:*.dll) ;;
    *) echo "error: platform/library mapping is invalid" >&2; return 1 ;;
  esac
  printf 'build-gdext self-test: %s/%s -> %s\n' "$platform" "$arch" "$library"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit 0
fi

profile="${1:-debug}"
case "$profile" in
  debug) cargo_profile_args=() ;;
  release) cargo_profile_args=(--release) ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

platform_details
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
target_dir_setting="${CARGO_TARGET_DIR:-}"
if [[ -n "$target_dir_setting" && "$target_dir_setting" != /* ]]; then
  target_dir="$repo_root/$target_dir_setting"
else
  target_dir="${target_dir_setting:-$repo_root/target}"
fi
source_library="$target_dir/$profile/$library"
destination_dir="$repo_root/game/bin/$platform"
cargo_bin="${CARGO:-cargo}"

printf 'Building spurfire-gdext (%s) for %s/%s...\n' "$profile" "$platform" "$arch"
"$cargo_bin" build --locked -p spurfire-gdext "${cargo_profile_args[@]}"

if [[ ! -f "$source_library" ]]; then
  echo "error: Cargo succeeded but $source_library was not produced" >&2
  exit 1
fi

mkdir -p "$destination_dir"
cp -f "$source_library" "$destination_dir/$library"
printf 'Installed %s\n' "$destination_dir/$library"
