#!/usr/bin/env bash
# Run two independently packaged clients through a caller-supplied simulated test driver.
# This entry point never provisions a provider resource and refuses credentialed environments.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
usage: scripts/run-alpha-two-client.sh \
  --client-a <archive> --client-b <archive> --driver <executable> --output <evidence.json>

The driver receives CLIENT_A, CLIENT_B, SOURCE_SHA, and EVIDENCE_OUTPUT as environment
variables and must write schema-v1 simulated lifecycle evidence. Credentials are forbidden.
Private-live mutation proof uses a separately reviewed, explicitly authorized harness; this
script cannot enable it.
EOF
}

client_a=''
client_b=''
driver=''
output=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --client-a) client_a=${2:-}; shift 2 ;;
    --client-b) client_b=${2:-}; shift 2 ;;
    --driver) driver=${2:-}; shift 2 ;;
    --output) output=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[[ -n "$client_a" && -n "$client_b" && -n "$driver" && -n "$output" ]] || { usage >&2; exit 2; }
[[ -s "$client_a" && -s "$client_b" ]] || { echo "error: both client archives must be nonempty" >&2; exit 1; }
[[ -x "$driver" ]] || { echo "error: test driver is not executable: $driver" >&2; exit 1; }

for name in TS_CLIENT_ID TS_CLIENT_SECRET TS_AUTHKEY TS_API_TOKEN SPURFIRE_CAPABILITY SPURFIRE_JOIN_CODE; do
  if [[ -n "${!name:-}" ]]; then
    echo "error: $name must be unset for simulated two-client qualification" >&2
    exit 1
  fi
done

source_sha="${SPURFIRE_ALPHA_SOURCE_SHA:-}"
if [[ -z "$source_sha" ]]; then
  source_sha="$(git -C "$repo_root" rev-parse HEAD)"
fi
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]] || {
  echo "error: SPURFIRE_ALPHA_SOURCE_SHA must be a full lowercase Git SHA" >&2
  exit 1
}
mkdir -p "$(dirname "$output")"
rm -f "$output"

CLIENT_A="$(cd "$(dirname "$client_a")" && pwd)/$(basename "$client_a")" \
CLIENT_B="$(cd "$(dirname "$client_b")" && pwd)/$(basename "$client_b")" \
SOURCE_SHA="$source_sha" \
EVIDENCE_OUTPUT="$(cd "$(dirname "$output")" && pwd)/$(basename "$output")" \
SPURFIRE_ALPHA_TEST_MODE=simulated \
  "$driver"

test -s "$output" || { echo "error: driver did not write lifecycle evidence" >&2; exit 1; }
python3 "$repo_root/scripts/check-alpha-lifecycle-evidence.py" "$output"
echo "simulated two-client Alpha flow passed (not private-live release evidence)"
