#!/usr/bin/env bash
# Credential-free signed 6/8/12/16-player virtual scale and soak proof.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

for name in \
  TS_CLIENT_ID TS_CLIENT_SECRET TS_API_BASE TS_API_BASE_URL TS_API_TOKEN TS_AUTHKEY \
  TAILSCALE_AUTHKEY SPURFIRE_CAPABILITY SPURFIRE_JOIN_CODE GH_TOKEN GITHUB_TOKEN
do
  if [[ -n "${!name+x}" ]]; then
    echo "error: $name must be unset for the credential-free scale proof" >&2
    exit 1
  fi
done
while IFS= read -r name; do
  if [[ "$name" =~ ^SPURFIRE_.*(TOKEN|KEY)$ ]]; then
    echo "error: secret-like $name must be unset for the credential-free scale proof" >&2
    exit 1
  fi
done < <(compgen -e)

cargo_bin="${CARGO:-cargo}"
"$cargo_bin" build --release --locked --quiet -p spurfire-net --bin spurfire-local-scale-soak

proof_bin="${CARGO_TARGET_DIR:-target}/release/spurfire-local-scale-soak"
if [[ "$proof_bin" != /* ]]; then
  proof_bin="$repo_root/$proof_bin"
fi
if [[ ! -x "$proof_bin" ]]; then
  echo "error: scale proof binary was not built at $proof_bin" >&2
  exit 1
fi

proof_tmp="$(mktemp -d "${TMPDIR:-/tmp}/spurfire-local-scale-proof.XXXXXX")"
proof_log="$proof_tmp/proof.log"
mkdir -p "$proof_tmp/home" "$proof_tmp/tmp"
trap 'rm -rf "$proof_tmp"' EXIT

env -i \
  HOME="$proof_tmp/home" \
  TMPDIR="$proof_tmp/tmp" \
  LC_ALL=C \
  "$proof_bin" 2>&1 | tee "$proof_log"

for peers in 6 8 12 16; do
  marker="SPURFIRE_LOCAL_SCALE_CASE_OK peers=$peers "
  count="$(grep -Fc "$marker" "$proof_log" || true)"
  if [[ "$count" -ne 1 ]]; then
    echo "error: expected exactly one scale marker; found $count: $marker" >&2
    exit 1
  fi
done

final_marker='SPURFIRE_LOCAL_SCALE_SOAK_OK cases=6,8,12,16 virtual_minutes=15 packet_loss=modeled forced_relay=modeled fairness_gap_percent=0.00'
count="$(grep -Fxc "$final_marker" "$proof_log" || true)"
if [[ "$count" -ne 1 ]]; then
  echo "error: expected exactly one final scale marker; found $count" >&2
  exit 1
fi

if grep -Eq 'panicked at|thread .* panicked|ERROR:' "$proof_log"; then
  echo "error: scale proof emitted a panic or error" >&2
  exit 1
fi

echo 'credential-free signed scale proof passed'
