#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/test-godot.sh [--self-test]

Imports the Godot project and runs its M0 smoke test headlessly.
Environment:
  GODOT_BIN             Godot executable (auto-detected if unset)
  GODOT_TIMEOUT_SECONDS Per-command timeout, default 120
  GODOT_SMOKE_SCRIPT    Project-relative script, default res://tests/m0_smoke.gd
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
    sleep "$seconds"
    if kill -0 "$command_pid" 2>/dev/null; then
      echo "error: command exceeded ${seconds}s: $*" >&2
      kill -TERM "$command_pid" 2>/dev/null || true
      sleep 2
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
  if run_bounded 1 bash -c 'sleep 3'; then
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
smoke_script="${GODOT_SMOKE_SCRIPT:-res://tests/m0_smoke.gd}"
godot_bin="$(find_godot)"

if [[ ! "$timeout_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: GODOT_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 2
fi
if [[ ! -f "$project_dir/project.godot" ]]; then
  echo "error: missing $project_dir/project.godot" >&2
  exit 1
fi

printf 'Importing Godot project (timeout %ss)...\n' "$timeout_seconds"
run_bounded "$timeout_seconds" "$godot_bin" --headless --path "$project_dir" --import
printf 'Running %s (timeout %ss)...\n' "$smoke_script" "$timeout_seconds"
run_bounded "$timeout_seconds" "$godot_bin" --headless --path "$project_dir" --script "$smoke_script"
