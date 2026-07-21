#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/test-godot.sh [--self-test]

Loads the Godot project and runs its M0 smoke scene headlessly.
Environment:
  GODOT_BIN             Godot executable (auto-detected if unset)
  GODOT_TIMEOUT_SECONDS Per-command timeout, default 120
  GODOT_SMOKE_SCENE     Project-relative scene, default res://scenes/headless_smoke.tscn
  GODOT_SINGLE_THREADED_SCENE  Set to 1 to force scene-tree work onto the main thread
  GODOT_DISABLE_CRASH_HANDLER  Set to 1 to expose native faults to the platform dumper
EOF
}

find_godot() {
  if [[ -n "${GODOT_BIN:-}" ]]; then
    printf '%s\n' "$GODOT_BIN"
    return
  fi
  local candidate
  for candidate in godot4 godot godot4.exe godot.exe; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return
    fi
  done
  echo "error: Godot 4 not found; set GODOT_BIN" >&2
  return 1
}

run_bounded() {
  local seconds="$1"
  shift
  "$@" &
  local command_pid=$!
  (
    watchdog_sleep_pid=""
    stop_watchdog() {
      if [[ -n "$watchdog_sleep_pid" ]]; then
        kill "$watchdog_sleep_pid" 2>/dev/null || true
        wait "$watchdog_sleep_pid" 2>/dev/null || true
      fi
      exit 0
    }
    trap stop_watchdog TERM INT
    sleep "$seconds" &
    watchdog_sleep_pid=$!
    wait "$watchdog_sleep_pid" || exit 0
    watchdog_sleep_pid=""
    if kill -0 "$command_pid" 2>/dev/null; then
      echo "error: command exceeded ${seconds}s: $*" >&2
      kill -TERM "$command_pid" 2>/dev/null || true
      sleep 2 &
      watchdog_sleep_pid=$!
      wait "$watchdog_sleep_pid" || exit 0
      watchdog_sleep_pid=""
      kill -KILL "$command_pid" 2>/dev/null || true
    fi
  ) &
  local watchdog_pid=$!

  local status=0
  wait "$command_pid" || status=$?
  kill "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true
  return "$status"
}

self_test() {
  run_bounded 2 bash -c 'exit 0'
  if run_bounded 1 sleep 3; then
    echo "error: timeout self-test unexpectedly succeeded" >&2
    return 1
  fi
  echo "test-godot self-test: timeout enforcement passed"
}

case "${1:-}" in
  --self-test) self_test; exit 0 ;;
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) usage >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
project_dir="$repo_root/game"
timeout_seconds="${GODOT_TIMEOUT_SECONDS:-120}"
smoke_scene="${GODOT_SMOKE_SCENE:-res://scenes/headless_smoke.tscn}"
godot_bin="$(find_godot)"
godot_runtime_args=()
if [[ "${GODOT_SINGLE_THREADED_SCENE:-0}" == "1" ]]; then
  godot_runtime_args+=(--single-threaded-scene)
fi
if [[ "${GODOT_DISABLE_CRASH_HANDLER:-0}" == "1" ]]; then
  godot_runtime_args+=(--disable-crash-handler)
fi

if [[ ! "$timeout_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: GODOT_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 2
fi
if [[ ! -f "$project_dir/project.godot" ]]; then
  echo "error: missing $project_dir/project.godot" >&2
  exit 1
fi

# Runtime builds do not import source GLBs themselves. Generate the ignored import cache with the
# native descriptor temporarily disabled: Godot 4.7.1's macOS headless editor crashes after
# loading GDExtension during layout initialization, while asset import without it is stable.
import_marker="$project_dir/.godot/.spurfire-assets-imported"
needs_import=false
if [[ ! -f "$import_marker" ]]; then
  needs_import=true
elif find "$project_dir/assets" -type f \( -name '*.glb' -o -name '*.gltf' \) -newer "$import_marker" -print -quit 2>/dev/null | grep -q .; then
  needs_import=true
fi
if [[ "$needs_import" == true ]]; then
  descriptor="$project_dir/bin/spurfire.gdextension"
  disabled_descriptor="$descriptor.import-disabled"
  import_log="$(mktemp "${TMPDIR:-/tmp}/spurfire-godot-import.XXXXXX")"
  mv "$descriptor" "$disabled_descriptor"
  import_status=0
  run_bounded "$timeout_seconds" "$godot_bin" \
    "${godot_runtime_args[@]}" \
    --headless \
    --display-driver headless \
    --audio-driver Dummy \
    --path "$project_dir" \
    --import >"$import_log" 2>&1 || import_status=$?
  mv "$disabled_descriptor" "$descriptor"
  if [[ "$import_status" -ne 0 ]]; then
    cat "$import_log" >&2
    rm -f "$import_log"
    exit "$import_status"
  fi
  rm -f "$import_log"
  mkdir -p "$project_dir/.godot"
  printf 'res://bin/spurfire.gdextension\n' > "$project_dir/.godot/extension_list.cfg"
  touch "$import_marker"
fi

printf 'Running %s headlessly (timeout %ss)...\n' "$smoke_scene" "$timeout_seconds"
run_bounded "$timeout_seconds" "$godot_bin" \
  "${godot_runtime_args[@]}" \
  --headless \
  --display-driver headless \
  --audio-driver Dummy \
  --path "$project_dir" \
  --scene "$smoke_scene"

for extra_scene in res://ui/tests/polish_smoke.tscn res://combat/tests/combat_smoke.tscn res://lobby/tests/lobby_contract_test.tscn; do
  printf 'Running %s...\n' "$extra_scene"
  run_bounded "$timeout_seconds" "$godot_bin" \
    "${godot_runtime_args[@]}" \
    --headless \
    --display-driver headless \
    --audio-driver Dummy \
    --path "$project_dir" \
    --scene "$extra_scene"
done

# The dedicated smoke scene validates the native class and course contract, while a short run of
# the configured main scene catches bootstrap/deferred-scene-change errors that the smoke scene
# intentionally bypasses.
runtime_log="$(mktemp "${TMPDIR:-/tmp}/spurfire-godot-runtime.XXXXXX")"
trap 'rm -f "$runtime_log"' EXIT
printf 'Running configured main scene for 30 frames...\n'
run_bounded "$timeout_seconds" "$godot_bin" \
  "${godot_runtime_args[@]}" \
  --headless \
  --display-driver headless \
  --audio-driver Dummy \
  --path "$project_dir" \
  --quit-after 30 2>&1 | tee "$runtime_log"
if grep -Eq 'ERROR:|SCRIPT ERROR|Parse Error' "$runtime_log"; then
  echo "error: Godot main-scene smoke emitted an error" >&2
  exit 1
fi
