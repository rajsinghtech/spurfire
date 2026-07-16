#!/usr/bin/env bash
set -euo pipefail
# Prevent inherited xtrace from exposing curl arguments containing credentials.
set +x

SCRIPT_DIR="${BASH_SOURCE[0]%/*}"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="$REPO_ROOT/.env"

if [[ ! -f "$ENV_FILE" ]]; then
  printf 'error: missing %s\n' "$ENV_FILE" >&2
  exit 1
fi

# shellcheck disable=SC1090
set -a
source "$ENV_FILE"
set +a

: "${TS_CLIENT_ID:?TS_CLIENT_ID is required in .env}"
: "${TS_CLIENT_SECRET:?TS_CLIENT_SECRET is required in .env}"
: "${TS_API_BASE:?TS_API_BASE is required in .env}"
TS_API_BASE="${TS_API_BASE%/}"

REVEAL=false
POSITIONAL=()
for arg in "$@"; do
  if [[ "$arg" == "--reveal" ]]; then
    REVEAL=true
  else
    POSITIONAL+=("$arg")
  fi
done
set -- "${POSITIONAL[@]}"

usage() {
  printf '%s\n' \
    'Usage: scripts/ts-api.sh [--reveal] COMMAND [ARG]' \
    '' \
    'Commands:' \
    '  token                     Validate OAuth and show redacted token metadata' \
    '  create-tailnet NAME       Try the alpha tailnet creation endpoint' \
    '  delete-tailnet NAME       Delete a disposable tailnet by name' \
    '  create-key [TAILNET]      Mint a 5-minute tagged ephemeral auth key (default: -)' \
    '  list-devices [TAILNET]    List devices (default: -)' \
    '  delete-device ID          Delete a device by ID' \
    '' \
    'Auth keys are redacted unless --reveal is explicitly supplied. OAuth tokens and' \
    'client credentials are never printed.' >&2
  exit 2
}

urlencode() {
  python3 - "$1" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
PY
}

redact_json() {
  local reveal_keys="$1"
  python3 -c '
import json, sys
reveal = sys.argv[1] == "true"
raw = sys.stdin.read()
try:
    value = json.loads(raw)
except Exception:
    print(raw, end="")
    raise SystemExit(0)

def clean(value):
    if isinstance(value, dict):
        result = {}
        for key, item in value.items():
            lowered = key.lower()
            if lowered in {"access_token", "token", "client_secret", "secret"}:
                result[key] = "<redacted>"
            elif lowered in {"key", "authkey", "auth_key"} and not reveal:
                result[key] = "<redacted-auth-key>"
            else:
                result[key] = clean(item)
        return result
    if isinstance(value, list):
        return [clean(item) for item in value]
    return value

print(json.dumps(clean(value), indent=2, sort_keys=True))
' "$reveal_keys"
}

split_response() {
  local response="$1"
  HTTP_STATUS="${response##*$'\n'__TS_STATUS__:}"
  HTTP_BODY="${response%$'\n'__TS_STATUS__:*}"
}

fetch_token() {
  local response
  response="$(curl --silent --show-error \
    --request POST \
    --header 'Accept: application/json' \
    --header 'Content-Type: application/x-www-form-urlencoded' \
    --data-urlencode 'grant_type=client_credentials' \
    --data-urlencode "client_id=$TS_CLIENT_ID" \
    --data-urlencode "client_secret=$TS_CLIENT_SECRET" \
    --write-out $'\n__TS_STATUS__:%{http_code}' \
    "$TS_API_BASE/oauth/token")"
  split_response "$response"
  if [[ ! "$HTTP_STATUS" =~ ^2 ]]; then
    printf 'HTTP %s\n' "$HTTP_STATUS" >&2
    printf '%s' "$HTTP_BODY" | redact_json false >&2
    exit 1
  fi
  TOKEN="$(printf '%s' "$HTTP_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')"
}

api_request() {
  local method="$1" path="$2" body="${3-}" response
  local curl_args=(
    --silent --show-error
    --request "$method"
    --header 'Accept: application/json'
    --header "Authorization: Bearer $TOKEN"
    --write-out $'\n__TS_STATUS__:%{http_code}'
  )
  if [[ -n "$body" ]]; then
    curl_args+=(--header 'Content-Type: application/json' --data "$body")
  fi
  response="$(curl "${curl_args[@]}" "$TS_API_BASE$path")"
  split_response "$response"
  printf 'HTTP %s\n' "$HTTP_STATUS"
  if [[ -n "$HTTP_BODY" ]]; then
    printf '%s' "$HTTP_BODY" | redact_json "$REVEAL"
  else
    printf '<empty body>\n'
  fi
  [[ "$HTTP_STATUS" =~ ^2 ]]
}

[[ $# -ge 1 ]] || usage
COMMAND="$1"
shift

case "$COMMAND" in
  token)
    [[ $# -eq 0 ]] || usage
    fetch_token
    printf 'HTTP %s\n' "$HTTP_STATUS"
    printf '%s' "$HTTP_BODY" | redact_json false
    ;;
  create-tailnet)
    [[ $# -eq 1 ]] || usage
    fetch_token
    body="$(python3 -c 'import json,sys; print(json.dumps({"name":sys.argv[1]}, separators=(",",":")))' "$1")"
    api_request POST '/tailnet' "$body"
    ;;
  delete-tailnet)
    [[ $# -eq 1 && "$1" != "-" ]] || usage
    fetch_token
    api_request DELETE "/tailnet/$(urlencode "$1")"
    ;;
  create-key)
    [[ $# -le 1 ]] || usage
    tailnet="${1--}"
    fetch_token
    body='{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":true,"preauthorized":true,"tags":["tag:spurfire-probe"]}}},"expirySeconds":300}'
    api_request POST "/tailnet/$(urlencode "$tailnet")/keys" "$body"
    ;;
  list-devices)
    [[ $# -le 1 ]] || usage
    tailnet="${1--}"
    fetch_token
    api_request GET "/tailnet/$(urlencode "$tailnet")/devices"
    ;;
  delete-device)
    [[ $# -eq 1 ]] || usage
    fetch_token
    api_request DELETE "/device/$(urlencode "$1")"
    ;;
  *)
    usage
    ;;
esac
