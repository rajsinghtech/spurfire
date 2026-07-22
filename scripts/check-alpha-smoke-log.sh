#!/usr/bin/env bash
# Assert that Godot exercised gameplay regressions and the one-lobby Alpha flow.
set -euo pipefail

usage() {
  echo "usage: scripts/check-alpha-smoke-log.sh [--allow-macos-dummy-shader-leak] <godot-log>" >&2
}

allow_macos_dummy_shader_leak=false
if [[ "${1:-}" == "--allow-macos-dummy-shader-leak" ]]; then
  allow_macos_dummy_shader_leak=true
  shift
fi
[[ $# -eq 1 ]] || { usage; exit 2; }
log=$1
[[ -f "$log" ]] || { echo "error: missing Godot log: $log" >&2; exit 1; }

allowed_macos_error_one="ERROR: 1 RID allocations of type 'N13RendererDummy15MaterialStorage11DummyShaderE' were leaked at exit."
allowed_macos_error_two="ERROR: 2 RID allocations of type 'N13RendererDummy15MaterialStorage11DummyShaderE' were leaked at exit."
allowed_macos_error_three="ERROR: 3 RID allocations of type 'N13RendererDummy15MaterialStorage11DummyShaderE' were leaked at exit."
while IFS= read -r line; do
  line=${line%$'\r'}
  case "$line" in
    *'ERROR:'*|*'SCRIPT ERROR'*|*'Parse Error'*|*'SMOKE:'*|*'ObjectDB instance'*|*'Leaked instance:'*)
      if [[ "$allow_macos_dummy_shader_leak" == true ]] \
        && { [[ "$line" == "$allowed_macos_error_one" ]] \
          || [[ "$line" == "$allowed_macos_error_two" ]] \
          || [[ "$line" == "$allowed_macos_error_three" ]]; }; then
        continue
      fi
      echo "error: Godot qualification log contains an engine or smoke failure" >&2
      echo "$line" >&2
      exit 1
      ;;
  esac
done < "$log"

required=(
  SPURFIRE_GODOT_SMOKE_OK
  SPURFIRE_POLISH_SMOKE_OK
  SPURFIRE_COMBAT_UI_SMOKE_OK
  SPURFIRE_ALPHA_LOBBY_SMOKE_OK
  SPURFIRE_OFFLINE_ALPHA_SMOKE_OK
)
for marker in "${required[@]}"; do
  count=$(grep -Fxc "$marker" "$log" || true)
  if [[ "$count" -ne 1 ]]; then
    echo "error: expected exactly one $marker marker; found $count" >&2
    exit 1
  fi
done

echo "alpha Godot smoke markers passed"
