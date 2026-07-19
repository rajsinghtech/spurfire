#!/usr/bin/env bash
# Fail closed if bearer material or legacy secret APIs re-enter Godot resources.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

pattern='auth_key|enrollment_key|creator_capability|participant_capability|Spurfire-Capability|connect_rustscale|make_join_code|parse_join_code|clipboard_get|clipboard_set'
mapfile -d '' files < <(find game -type f \( -name '*.gd' -o -name '*.tscn' \) -print0 | sort -z)
if ((${#files[@]} == 0)); then
  echo 'error: no Godot sources found for native-secret boundary check' >&2
  exit 1
fi
if grep -nE "$pattern" "${files[@]}"; then
  echo 'error: a bearer identifier or forbidden secret API entered a Godot source' >&2
  exit 1
fi
if find game -type f -name 'lobby_http_client.gd' -print -quit | grep -q .; then
  echo 'error: legacy scripted lobby HTTP client exists' >&2
  exit 1
fi
if ! grep -Fq 'type="NativeSecretInput"' game/lobby/lobby_shell.tscn; then
  echo 'error: lobby scene does not use native masked input' >&2
  exit 1
fi
if ! grep -Fq 'session.has_method(&("connect_" + "rustscale"))' game/lobby/tests/lobby_contract_test.gd; then
  echo 'error: Godot contract does not assert removal of the secret transport ABI' >&2
  exit 1
fi

echo 'Godot native-secret boundary check passed'
