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
    Darwin) platform="macos"; source_library="libspurfire_godot.dylib"; extension="dylib" ;;
    Linux) platform="linux"; source_library="libspurfire_godot.so"; extension="so" ;;
    MINGW*|MSYS*|CYGWIN*) platform="windows"; source_library="spurfire_godot.dll"; extension="dll" ;;
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
  case "$platform:$source_library" in
    macos:*.dylib|linux:*.so|windows:*.dll) ;;
    *) echo "error: platform/library mapping is invalid" >&2; return 1 ;;
  esac
  printf 'build-gdext self-test: %s/%s -> %s\n' "$platform" "$arch" "$source_library"
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
source_path="$target_dir/$profile/$source_library"
destination_dir="$repo_root/game/bin/$platform"
if [[ "$platform" == "windows" ]]; then
  destination_library="spurfire_godot.$profile.$arch.$extension"
else
  destination_library="libspurfire_godot.$profile.$arch.$extension"
fi
cargo_bin="${CARGO:-cargo}"

printf 'Building spurfire-gdext (%s) for %s/%s...\n' "$profile" "$platform" "$arch"
# After a Rust source/layout change, Cargo's incremental macOS cdylib relink can occasionally
# produce a Mach-O that passes codesign verification but is killed by the loader. Cleaning only
# this package is fast and prevents copying that stale artifact into Godot.
if [[ "$platform" == "macos" ]]; then
  "$cargo_bin" clean -p spurfire-gdext
fi
"$cargo_bin" build --locked -p spurfire-gdext "${cargo_profile_args[@]}"

if [[ ! -f "$source_path" ]]; then
  echo "error: Cargo succeeded but $source_path was not produced" >&2
  exit 1
fi

mkdir -p "$destination_dir"
destination_path="$destination_dir/$destination_library"
cp -f "$source_path" "$destination_path"
if [[ "$platform" == "macos" ]]; then
  # Renaming an ad-hoc signed Mach-O can leave a signature that verifies on disk but is killed by
  # library validation at dlopen time. Re-sign the final path Godot will load.
  codesign --force --sign - "$destination_path"
fi
printf 'Installed %s\n' "$destination_path"
