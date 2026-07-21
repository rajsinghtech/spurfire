#!/usr/bin/env bash
# Open interactive Godot clients, qualify an eight-process HUD matrix, or run
# its changing-transform soak on one disposable RustScale tailnet.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="interactive"
case "${1:-}" in
  "") ;;
  --qualify) MODE="qualify" ;;
  --soak) MODE="soak" ;;
  *) echo "usage: $0 [--qualify|--soak]" >&2; exit 2 ;;
esac

if [[ "$MODE" == "qualify" || "$MODE" == "soak" ]]; then
  NODES=(a b c d e f g h)
else
  NODES=(a b c)
fi
NODE_CSV=$(IFS=,; echo "${NODES[*]}")

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi
: "${TS_CLIENT_ID:?TS_CLIENT_ID is required in .env}"
: "${TS_CLIENT_SECRET:?TS_CLIENT_SECRET is required in .env}"
command -v curl >/dev/null
command -v jq >/dev/null
GODOT_BIN="${GODOT_BIN:-$(command -v godot4 || command -v godot || true)}"
[[ -n "$GODOT_BIN" ]] || { echo "Godot 4 was not found; set GODOT_BIN" >&2; exit 1; }
SOAK_MS=0
if [[ "$MODE" == "soak" ]]; then
  SOAK_MS="${SPURFIRE_P2P_SOAK_MS:-900000}"
  [[ "$SOAK_MS" =~ ^[1-9][0-9]*$ ]] || { echo "SPURFIRE_P2P_SOAK_MS must be a positive integer" >&2; exit 2; }
fi

PIN_CLIENTS=0
if [[ "$MODE" == "qualify" || "$MODE" == "soak" ]]; then
  if [[ "${SPURFIRE_P2P_PIN_CLIENTS:-0}" == "1" ]] \
    && command -v taskset >/dev/null \
    && [[ "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 0)" -ge "${#NODES[@]}" ]]; then
    PIN_CLIENTS=1
    printf 'pinning each Godot/RustScale client pair to a dedicated CPU\n' >&2
  fi
fi

# Build before provisioning so a compiler failure cannot leave a live child tailnet.
scripts/build-gdext.sh debug

API="${TS_API_BASE:-${TS_API_BASE_URL:-https://api.tailscale.com}}"
API="${API%/api/v2}"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/spurfire-godot-p2p.XXXXXX")
chmod 700 "$TMP"
CREDS="$TMP/child.json"
PIDS=()
DELETED=0

cleanup() {
  local status=$? token dns cid secret code pid
  trap - EXIT INT TERM
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [[ -f "$CREDS" && "$DELETED" -eq 0 ]]; then
    dns=$(jq -r '.dnsName // empty' "$CREDS")
    cid=$(jq -r '.clientId // empty' "$CREDS")
    secret=$(jq -r '.clientSecret // empty' "$CREDS")
    token=$(curl -fsS --retry 8 --retry-delay 2 --retry-all-errors \
      -X POST "$API/api/v2/oauth/token" \
      -d client_id="$cid" -d client_secret="$secret" 2>/dev/null | jq -r '.access_token // empty' || true)
    if [[ -n "$token" && -n "$dns" ]]; then
      code=$(curl -sS --retry 8 --retry-delay 2 --retry-all-errors -o /dev/null -w '%{http_code}' \
        -X DELETE "$API/api/v2/tailnet/$dns" -H "Authorization: Bearer $token" || true)
      if [[ "$code" == "200" || "$code" == "204" || "$code" == "404" ]]; then
        echo "cleanup: deleted P2P demo tailnet $dns" >&2
        DELETED=1
      else
        echo "ERROR: cleanup returned HTTP $code for $dns" >&2
        status=1
      fi
    else
      echo "ERROR: cleanup could not mint the child token for $dns" >&2
      status=1
    fi
  fi
  if [[ ! -f "$CREDS" || "$DELETED" -eq 1 ]]; then
    rm -rf "$TMP"
  else
    chmod 700 "$TMP"
    echo "RECOVERY REQUIRED: retained private child credentials at $TMP" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

ORG_TOKEN=$(curl -fsS --retry 3 --retry-delay 2 -X POST "$API/api/v2/oauth/token" \
  -d client_id="$TS_CLIENT_ID" -d client_secret="$TS_CLIENT_SECRET" | jq -r '.access_token // empty')
[[ -n "$ORG_TOKEN" ]] || { echo "failed to mint organization token" >&2; exit 1; }
NAME="spurfire-godot-$(date +%s)"
CREATED=$(curl -fsS --retry 5 --retry-delay 3 --retry-all-errors \
  -X POST "$API/api/v2/organizations/-/tailnets" \
  -H "Authorization: Bearer $ORG_TOKEN" -H 'Content-Type: application/json' \
  --data "$(jq -nc --arg name "$NAME" '{displayName:$name}')")
DNS=$(jq -r '.dnsName // empty' <<<"$CREATED")
CHILD_CID=$(jq -r '.oauthClient.id // empty' <<<"$CREATED")
CHILD_CSEC=$(jq -r '.oauthClient.secret // empty' <<<"$CREATED")
[[ -n "$DNS" && -n "$CHILD_CID" && -n "$CHILD_CSEC" ]] || { echo "tailnet create response was incomplete" >&2; exit 1; }
jq -n --arg dns "$DNS" --arg cid "$CHILD_CID" --arg secret "$CHILD_CSEC" \
  '{dnsName:$dns,clientId:$cid,clientSecret:$secret}' >"$CREDS"
chmod 600 "$CREDS"
unset CREATED TS_CLIENT_SECRET
printf 'created P2P demo tailnet: %s\n' "$DNS" >&2

CHILD_TOKEN=$(curl -fsS -X POST "$API/api/v2/oauth/token" \
  -d client_id="$CHILD_CID" -d client_secret="$CHILD_CSEC" | jq -r '.access_token // empty')
unset CHILD_CSEC
curl -fsS --retry 3 --retry-delay 2 --retry-all-errors \
  -X POST "$API/api/v2/tailnet/$DNS/acl" \
  -H "Authorization: Bearer $CHILD_TOKEN" -H 'Content-Type: application/json' \
  -d '{"tagOwners":{"tag:spurfire":[]},"acls":[{"action":"accept","src":["*"],"dst":["*:*"]}]}' >/dev/null

mint_key() {
  local destination=$1
  curl -fsS --retry 3 --retry-delay 2 --retry-all-errors \
    -X POST "$API/api/v2/tailnet/$DNS/keys" \
    -H "Authorization: Bearer $CHILD_TOKEN" -H 'Content-Type: application/json' \
    -d '{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":true,"preauthorized":true,"tags":["tag:spurfire"]}}},"expirySeconds":900}' \
    | jq -er '.key' >"$destination"
  chmod 600 "$destination"
}
for node in "${NODES[@]}"; do
  mint_key "$TMP/key-$node"
done
unset CHILD_TOKEN ORG_TOKEN

if [[ "$MODE" == "interactive" ]]; then
  printf '\nLaunching three Godot peers. Focus any window and ride; both other windows should show that horse.\n'
  printf 'Hold TAB for route/RTT. Close all windows or press Ctrl-C here to delete the disposable tailnet.\n\n'
fi

for index in "${!NODES[@]}"; do
  node="${NODES[$index]}"
  common_env=(
    "SPURFIRE_P2P_DEMO=1"
    "SPURFIRE_P2P_DEMO_NODE=$node"
    "SPURFIRE_P2P_DEMO_NODES=$NODE_CSV"
    "SPURFIRE_P2P_DEMO_DIR=$TMP"
    "SPURFIRE_P2P_DEMO_KEY_FILE=$TMP/key-$node"
  )
  if [[ "$MODE" == "qualify" || "$MODE" == "soak" ]]; then
    timeout_ms=$((SOAK_MS + 150000))
    qualify_env=(
      "${common_env[@]}"
      "SPURFIRE_P2P_DEMO_QUALIFY=1"
      "SPURFIRE_P2P_DEMO_SOAK_MS=$SOAK_MS"
      "SPURFIRE_P2P_DEMO_TIMEOUT_MS=$timeout_ms"
    )
    if [[ "$PIN_CLIENTS" -eq 1 ]]; then
      env "${qualify_env[@]}" taskset --cpu-list "$index" \
        "$GODOT_BIN" --headless --path game >"$TMP/client-$node.log" 2>&1 &
    else
      env "${qualify_env[@]}" \
        "$GODOT_BIN" --headless --path game >"$TMP/client-$node.log" 2>&1 &
    fi
  else
    x=$((20 + (index % 3) * 640))
    y=$((50 + (index / 3) * 460))
    env "${common_env[@]}" "$GODOT_BIN" --path game --resolution 620x430 --position "$x,$y" &
  fi
  PIDS+=("$!")
  # Avoid phase-locking eight 60 Hz scene loops and their once-per-second
  # probe bursts on a single qualification host. RustScale independently
  # randomizes its periodic endpoint maintenance interval.
  if [[ "$MODE" == "qualify" || "$MODE" == "soak" ]]; then
    launch_stagger_sec="${SPURFIRE_P2P_LAUNCH_STAGGER_SEC:-0.15}"
    sleep "$launch_stagger_sec"
  fi
done

wait_status=0
for index in "${!PIDS[@]}"; do
  pid="${PIDS[$index]}"
  if wait "$pid"; then
    :
  else
    client_status=$?
    printf 'SPURFIRE_GODOT_P2P_PROCESS_FAILED node=%s status=%d\n' \
      "${NODES[$index]}" "$client_status" >&2
    wait_status=1
  fi
done
PIDS=()

if [[ "$MODE" == "qualify" || "$MODE" == "soak" ]]; then
  if [[ "$wait_status" -ne 0 ]]; then
    for log in "$TMP"/client-*.log; do
      echo "--- ${log##*/}" >&2
      grep -E 'SPURFIRE_GODOT_P2P_(LONG_GAP|SOAK|QUALIFY_FAILED)|SCRIPT ERROR|Parse Error|ERROR:' "$log" \
        | tail -40 >&2 || true
    done
    exit 1
  fi
  if ! evidence=$(scripts/check-godot-p2p-evidence.py "$TMP" "$NODE_CSV" "$SOAK_MS" 2>&1); then
    printf '%s\n' "$evidence" >&2
    for log in "$TMP"/client-*.log; do
      echo "--- ${log##*/}" >&2
      grep -E 'SPURFIRE_GODOT_P2P_(SOAK|QUALIFY_FAILED|MEASURED|HUD)' "$log" \
        | tail -30 >&2 || true
    done
    exit 1
  fi
  printf '%s\n' "$evidence"
elif [[ "$wait_status" -ne 0 ]]; then
  exit 1
fi
