#!/usr/bin/env bash
# Credential-free signed two-peer and three-peer authority-migration process proofs.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

for name in \
  TS_CLIENT_ID TS_CLIENT_SECRET TS_API_BASE TS_API_BASE_URL TS_API_TOKEN TS_AUTHKEY \
  TAILSCALE_AUTHKEY SPURFIRE_CAPABILITY SPURFIRE_JOIN_CODE GH_TOKEN GITHUB_TOKEN
do
  if [[ -n "${!name+x}" ]]; then
    echo "error: $name must be unset for the credential-free process proof" >&2
    exit 1
  fi
done
while IFS= read -r name; do
  if [[ "$name" =~ ^SPURFIRE_.*(TOKEN|KEY)$ ]]; then
    echo "error: secret-like $name must be unset for the credential-free process proof" >&2
    exit 1
  fi
done < <(compgen -e)

cargo_bin="${CARGO:-cargo}"
"$cargo_bin" build --locked --quiet -p spurfire-net --bin spurfire-local-p2p-proof

proof_bin="${CARGO_TARGET_DIR:-target}/debug/spurfire-local-p2p-proof"
if [[ "$proof_bin" != /* ]]; then
  proof_bin="$repo_root/$proof_bin"
fi
if [[ ! -x "$proof_bin" ]]; then
  echo "error: signed process proof binary was not built at $proof_bin" >&2
  exit 1
fi

proof_tmp="$(mktemp -d "${TMPDIR:-/tmp}/spurfire-local-p2p-proof.XXXXXX")"
proof_log="$proof_tmp/proof.log"
mkdir -p "$proof_tmp/home" "$proof_tmp/tmp"
trap 'rm -rf "$proof_tmp"' EXIT

# The proof binary enforces bounded control/scenario deadlines and reaps every
# peer child. Keep compilation outside those deadlines and use only portable
# POSIX/macOS tooling for the strict runtime environment.
env -i \
  HOME="$proof_tmp/home" \
  TMPDIR="$proof_tmp/tmp" \
  LC_ALL=C \
  "$proof_bin" 2>&1 | tee "$proof_log"

required=(
  'SPURFIRE_SIGNED_TWO_PROCESS_OK peer_processes=2 signatures=strict accepted_bidirectional=true combat=authority_once result_dedup=true authority=a epoch=1'
  'SPURFIRE_SIGNED_THREE_PROCESS_MIGRATION_OK peer_processes=3 signatures=strict authority_roles=strict authority=a successor=b epoch=2 agreement=b,c checkpoint=hash_checked riders=2 combat_receipts=retained continued_play=true'
)
for marker in "${required[@]}"; do
  count="$(grep -Fxc "$marker" "$proof_log" || true)"
  if [[ "$count" -ne 1 ]]; then
    echo "error: expected exactly one signed process marker; found $count: $marker" >&2
    exit 1
  fi
done

if grep -Eq 'panicked at|thread .* panicked|ERROR:' "$proof_log"; then
  echo "error: signed process proof emitted a panic or error" >&2
  exit 1
fi

echo 'credential-free signed process proofs passed'
