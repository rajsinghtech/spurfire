#!/usr/bin/env bash
# Provisions an API-only child tailnet, runs two real RustScale UDP peers, and deletes it.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi
: "${TS_CLIENT_ID:?TS_CLIENT_ID is required}"
: "${TS_CLIENT_SECRET:?TS_CLIENT_SECRET is required}"
command -v curl >/dev/null
command -v jq >/dev/null

API="${TS_API_BASE_URL:-https://api.tailscale.com}"
API="${API%/api/v2}"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/spurfire-p2p.XXXXXX")
chmod 700 "$TMP"
CREDS="$TMP/child.json"
DELETED=0

cleanup() {
  local token dns cid secret code
  if [[ -f "$CREDS" && "$DELETED" -eq 0 ]]; then
    dns=$(jq -r '.dnsName // empty' "$CREDS")
    cid=$(jq -r '.clientId // empty' "$CREDS")
    secret=$(jq -r '.clientSecret // empty' "$CREDS")
    if [[ -n "$dns" && -n "$cid" && -n "$secret" ]]; then
      token=$(curl -fsS -X POST "$API/api/v2/oauth/token" \
        -d client_id="$cid" -d client_secret="$secret" 2>/dev/null | jq -r '.access_token // empty' || true)
      if [[ -n "$token" ]]; then
        code=$(curl -sS --retry 3 --retry-delay 2 -o /dev/null -w '%{http_code}' \
          -X DELETE "$API/api/v2/tailnet/$dns" -H "Authorization: Bearer $token" || true)
        if [[ "$code" == "200" || "$code" == "204" || "$code" == "404" ]]; then
          echo "cleanup: deleted child tailnet $dns" >&2
          DELETED=1
        else
          echo "ERROR: cleanup returned HTTP $code for $dns" >&2
        fi
      fi
    fi
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

ORG_TOKEN=$(curl -fsS --retry 3 --retry-delay 2 -X POST "$API/api/v2/oauth/token" \
  -d client_id="$TS_CLIENT_ID" -d client_secret="$TS_CLIENT_SECRET" | jq -r '.access_token // empty')
[[ -n "$ORG_TOKEN" ]] || { echo "failed to mint organization token" >&2; exit 1; }
NAME="spurfire-p2p-$(date +%s)"
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
printf 'created child tailnet: %s\n' "$DNS" >&2

CHILD_TOKEN=$(curl -fsS -X POST "$API/api/v2/oauth/token" \
  -d client_id="$CHILD_CID" -d client_secret="$CHILD_CSEC" | jq -r '.access_token // empty')
[[ -n "$CHILD_TOKEN" ]] || { echo "failed to mint child token" >&2; exit 1; }
unset CHILD_CSEC
curl -fsS --retry 3 --retry-delay 2 --retry-all-errors \
  -X POST "$API/api/v2/tailnet/$DNS/acl" \
  -H "Authorization: Bearer $CHILD_TOKEN" -H 'Content-Type: application/json' \
  -d '{"tagOwners":{"tag:spurfire":[]},"acls":[{"action":"accept","src":["*"],"dst":["*:*"]}]}' >/dev/null

mint_key() {
  local destination=$1 response
  response=$(curl -fsS --retry 3 --retry-delay 2 --retry-all-errors \
    -X POST "$API/api/v2/tailnet/$DNS/keys" \
    -H "Authorization: Bearer $CHILD_TOKEN" -H 'Content-Type: application/json' \
    -d '{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":true,"preauthorized":true,"tags":["tag:spurfire"]}}},"expirySeconds":900}')
  jq -er '.key' <<<"$response" >"$destination"
  chmod 600 "$destination"
}
mint_key "$TMP/key-a"
mint_key "$TMP/key-b"
mint_key "$TMP/derp-a"
mint_key "$TMP/derp-b"
mint_key "$TMP/migration-a"
mint_key "$TMP/migration-b"
mint_key "$TMP/migration-c"
unset CHILD_TOKEN ORG_TOKEN

cargo run --locked --quiet -p spurfire-net --features rustscale-test-support --bin spurfire-p2p-smoke -- \
  --key-a "$TMP/key-a" --key-b "$TMP/key-b"
cargo run --locked --quiet -p spurfire-net --features rustscale-test-support --bin spurfire-p2p-smoke -- \
  --force-derp --key-a "$TMP/derp-a" --key-b "$TMP/derp-b"
mkdir "$TMP/migration"
cargo run --locked --quiet -p spurfire-net --features rustscale --bin spurfire-migration-smoke -- \
  --key-a "$TMP/migration-a" --key-b "$TMP/migration-b" --key-c "$TMP/migration-c" \
  --dir "$TMP/migration"

# Cleanup now (the trap remains a fallback) and fail if exact deletion did not succeed.
cleanup
[[ "$DELETED" -eq 1 ]] || exit 1
trap - EXIT INT TERM
printf 'SPURFIRE_P2P_LIFECYCLE_OK tailnet=%s\n' "$DNS"
