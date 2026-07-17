#!/usr/bin/env bash
# Open two Godot clients on one disposable RustScale tailnet.
set -euo pipefail
cd "$(dirname "$0")/.."

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

# Build before provisioning so a compiler failure cannot leave a live child tailnet.
scripts/build-gdext.sh debug

API="${TS_API_BASE_URL:-https://api.tailscale.com}"
API="${API%/api/v2}"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/spurfire-godot-p2p.XXXXXX")
chmod 700 "$TMP"
CREDS="$TMP/child.json"
PID_A=""
PID_B=""
DELETED=0

cleanup() {
  local token dns cid secret code
  [[ -z "$PID_A" ]] || kill "$PID_A" 2>/dev/null || true
  [[ -z "$PID_B" ]] || kill "$PID_B" 2>/dev/null || true
  if [[ -f "$CREDS" && "$DELETED" -eq 0 ]]; then
    dns=$(jq -r '.dnsName // empty' "$CREDS")
    cid=$(jq -r '.clientId // empty' "$CREDS")
    secret=$(jq -r '.clientSecret // empty' "$CREDS")
    token=$(curl -fsS -X POST "$API/api/v2/oauth/token" \
      -d client_id="$cid" -d client_secret="$secret" 2>/dev/null | jq -r '.access_token // empty' || true)
    if [[ -n "$token" && -n "$dns" ]]; then
      code=$(curl -sS --retry 3 --retry-delay 2 -o /dev/null -w '%{http_code}' \
        -X DELETE "$API/api/v2/tailnet/$dns" -H "Authorization: Bearer $token" || true)
      if [[ "$code" == "200" || "$code" == "204" || "$code" == "404" ]]; then
        echo "cleanup: deleted P2P demo tailnet $dns" >&2
        DELETED=1
      else
        echo "ERROR: cleanup returned HTTP $code for $dns" >&2
      fi
    fi
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

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
mint_key "$TMP/key-a"
mint_key "$TMP/key-b"
unset CHILD_TOKEN ORG_TOKEN

printf '\nLaunching two Godot peers. Focus either window and ride; the other window should show the remote horse.\n'
printf 'Close both windows or press Ctrl-C here to delete the disposable tailnet.\n\n'
SPURFIRE_P2P_DEMO=1 SPURFIRE_P2P_DEMO_NODE=a SPURFIRE_P2P_DEMO_DIR="$TMP" \
SPURFIRE_P2P_DEMO_KEY_FILE="$TMP/key-a" \
  "$GODOT_BIN" --path game --resolution 900x600 --position 30,60 &
PID_A=$!
SPURFIRE_P2P_DEMO=1 SPURFIRE_P2P_DEMO_NODE=b SPURFIRE_P2P_DEMO_DIR="$TMP" \
SPURFIRE_P2P_DEMO_KEY_FILE="$TMP/key-b" \
  "$GODOT_BIN" --path game --resolution 900x600 --position 970,60 &
PID_B=$!

wait "$PID_A" "$PID_B"
PID_A=""
PID_B=""
