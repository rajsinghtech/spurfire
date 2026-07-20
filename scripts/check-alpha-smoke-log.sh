#!/usr/bin/env bash
# Assert that Linux Godot exercised both gameplay regressions and the one-lobby Alpha flow.
set -euo pipefail

usage() {
  echo "usage: scripts/check-alpha-smoke-log.sh <godot-log>" >&2
}

[[ $# -eq 1 ]] || { usage; exit 2; }
log=$1
[[ -f "$log" ]] || { echo "error: missing Godot log: $log" >&2; exit 1; }

if grep -Eq 'ERROR:|SCRIPT ERROR|Parse Error|SMOKE:' "$log"; then
  echo "error: Godot qualification log contains an engine or smoke failure" >&2
  exit 1
fi

required=(
  SPURFIRE_GODOT_SMOKE_OK
  SPURFIRE_POLISH_SMOKE_OK
  SPURFIRE_COMBAT_UI_SMOKE_OK
  SPURFIRE_ALPHA_LOBBY_SMOKE_OK
)
for marker in "${required[@]}"; do
  count=$(grep -Fxc "$marker" "$log" || true)
  if [[ "$count" -ne 1 ]]; then
    echo "error: expected exactly one $marker marker; found $count" >&2
    exit 1
  fi
done

echo "alpha Godot smoke markers passed"
