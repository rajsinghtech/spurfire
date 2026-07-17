#!/usr/bin/env bash
set -euo pipefail
# Never allow an inherited xtrace setting to expose OAuth form bodies.
set +x

SCRIPT_DIR="${BASH_SOURCE[0]%/*}"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="$REPO_ROOT/.env"
HTTP_STATUS=""
HTTP_BODY=""
TOKEN=""
CHILD_ID=""
CHILD_SECRET=""
CHILD_TOKEN=""
CHILD_DNS=""
CHILD_STABLE_ID=""
PROBE_NAME=""
CREATED=false
CLEANED=false

usage() {
  cat >&2 <<'EOF'
Usage: scripts/ts-api.sh COMMAND [OPTION]

Read-only commands:
  token                         Validate organization OAuth; print metadata only
  list-tailnets                 Print only the organization tailnet count

Guarded mutation:
  probe-org-tailnet --confirm   Create one spurfire-probe-* child, exchange its
                                child token, delete it, and verify its stable ID
                                is absent from the organization listing

Local validation:
  self-test                     Exercise non-network safety helpers

OAuth tokens and child OAuth credentials are never printed. The probe installs
its cleanup trap before creation and treats failed cleanup as an error.
EOF
  exit 2
}

urlencode() {
  python3 - "$1" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
PY
}

split_response() {
  local response="$1"
  HTTP_STATUS="${response##*$'\n'__TS_STATUS__:}"
  HTTP_BODY="${response%$'\n'__TS_STATUS__:*}"
}

form_body() {
  # Keep values out of the Python process argument list.
  printf '%s\0' "$@" | python3 -c '
import sys
from urllib.parse import urlencode
items = sys.stdin.buffer.read().split(b"\0")
pairs = []
for raw in items:
    if not raw:
        continue
    key, value = raw.decode().split("=", 1)
    pairs.append((key, value))
print(urlencode(pairs), end="")
'
}

load_environment() {
  if [[ ! -f "$ENV_FILE" ]]; then
    printf 'error: missing %s\n' "$ENV_FILE" >&2
    exit 1
  fi
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
  : "${TS_CLIENT_ID:?TS_CLIENT_ID is required in .env}"
  : "${TS_CLIENT_SECRET:?TS_CLIENT_SECRET is required in .env}"
  : "${TS_API_BASE:?TS_API_BASE is required in .env}"
  TS_API_BASE="${TS_API_BASE%/}"
}

fetch_token_for() {
  local client_id="$1" client_secret="$2" response form
  form="$(form_body \
    'grant_type=client_credentials' \
    "client_id=$client_id" \
    "client_secret=$client_secret")"
  response="$(printf '%s' "$form" | curl --silent --show-error \
    --request POST \
    --header 'Accept: application/json' \
    --header 'Content-Type: application/x-www-form-urlencoded' \
    --data-binary @- \
    --write-out $'\n__TS_STATUS__:%{http_code}' \
    "$TS_API_BASE/oauth/token")"
  form=""
  split_response "$response"
  response=""
  if [[ ! "$HTTP_STATUS" =~ ^2 ]]; then
    printf 'error: OAuth token exchange failed (HTTP %s; body discarded)\n' "$HTTP_STATUS" >&2
    HTTP_BODY=""
    return 1
  fi
  local access_token
  access_token="$(printf '%s' "$HTTP_BODY" | python3 -c '
import json, sys
value = json.load(sys.stdin).get("access_token")
if not isinstance(value, str) or not value:
    raise SystemExit(1)
print(value, end="")
')" || {
    HTTP_BODY=""
    printf 'error: OAuth response had no access_token\n' >&2
    return 1
  }
  HTTP_BODY=""
  printf '%s' "$access_token"
}

fetch_org_token() {
  TOKEN="$(fetch_token_for "$TS_CLIENT_ID" "$TS_CLIENT_SECRET")"
  HTTP_STATUS=200
}

fetch_child_token() {
  CHILD_TOKEN="$(fetch_token_for "$CHILD_ID" "$CHILD_SECRET")"
  HTTP_STATUS=200
}

api_capture() {
  local method="$1" path="$2" body="${3-}" response
  local curl_args=(
    --silent --show-error
    --request "$method"
    --header 'Accept: application/json'
    --header "Authorization: Bearer $TOKEN"
    --write-out $'\n__TS_STATUS__:%{http_code}'
  )
  if [[ -n "$body" ]]; then
    curl_args+=(--header 'Content-Type: application/json' --data-binary @-)
    response="$(printf '%s' "$body" | curl "${curl_args[@]}" "$TS_API_BASE$path")"
  else
    response="$(curl "${curl_args[@]}" "$TS_API_BASE$path")"
  fi
  split_response "$response"
  response=""
}

list_tailnets_capture() {
  api_capture GET '/organizations/-/tailnets'
  if [[ ! "$HTTP_STATUS" =~ ^2 ]]; then
    printf 'error: organization tailnet list failed (HTTP %s; body discarded)\n' "$HTTP_STATUS" >&2
    HTTP_BODY=""
    return 1
  fi
}

cleanup_created() {
  [[ "$CREATED" == true && "$CLEANED" == false ]] || return 0
  printf 'cleanup: deleting guarded probe %s\n' "$PROBE_NAME" >&2
  if [[ -z "$CHILD_TOKEN" ]]; then
    if [[ -z "$CHILD_ID" || -z "$CHILD_SECRET" ]] || ! fetch_child_token; then
      printf 'error: cleanup requires manual remediation; child credentials are unavailable\n' >&2
      return 1
    fi
  fi

  local saved_token="$TOKEN" encoded response
  TOKEN="$CHILD_TOKEN"
  encoded="$(urlencode "$CHILD_DNS")"
  api_capture DELETE "/tailnet/$encoded"
  response="$HTTP_STATUS"
  HTTP_BODY=""
  TOKEN="$saved_token"
  saved_token=""
  case "$response" in
    2*|404)
      CLEANED=true
      CREATED=false
      CHILD_SECRET=""
      CHILD_TOKEN=""
      printf 'cleanup: deleted (HTTP %s)\n' "$response" >&2
      return 0
      ;;
    *)
      printf 'error: cleanup failed (HTTP %s; body discarded); manual remediation required\n' "$response" >&2
      return 1
      ;;
  esac
}

cleanup_on_exit() {
  local original_status=$?
  if ! cleanup_created; then
    exit 1
  fi
  exit "$original_status"
}

self_test() {
  local encoded sample
  encoded="$(urlencode 'tail name.ts.net')"
  [[ "$encoded" == 'tail%20name.ts.net' ]] || {
    printf 'self-test failed: URL encoding\n' >&2
    return 1
  }
  sample="$(form_body 'client_id=a b' 'client_secret=x+y')"
  [[ "$sample" == 'client_id=a+b&client_secret=x%2By' ]] || {
    printf 'self-test failed: form encoding\n' >&2
    return 1
  }
  PROBE_NAME="spurfire-probe-12345-678"
  [[ "$PROBE_NAME" == spurfire-probe-* && ${#PROBE_NAME} -le 50 ]] || {
    printf 'self-test failed: guarded prefix\n' >&2
    return 1
  }
  printf 'ts-api self-test: ok\n'
}

[[ $# -ge 1 ]] || usage
COMMAND="$1"
shift
case "$COMMAND" in
  help|-h|--help)
    usage
    ;;
  self-test)
    [[ $# -eq 0 ]] || usage
    self_test
    exit 0
    ;;
  token|list-tailnets|probe-org-tailnet)
    load_environment
    ;;
  *)
    usage
    ;;
esac

case "$COMMAND" in
  token)
    [[ $# -eq 0 ]] || usage
    fetch_org_token
    # Parse non-secret metadata only; the access token itself remains in TOKEN.
    printf 'OAuth token exchange: HTTP %s (access token redacted)\n' "$HTTP_STATUS"
    ;;
  list-tailnets)
    [[ $# -eq 0 ]] || usage
    fetch_org_token
    list_tailnets_capture
    count="$(printf '%s' "$HTTP_BODY" | python3 -c '
import json, sys
value = json.load(sys.stdin)
tailnets = value.get("tailnets")
if not isinstance(tailnets, list):
    raise SystemExit(1)
print(len(tailnets))
')" || {
      HTTP_BODY=""
      printf 'error: invalid organization tailnet list response\n' >&2
      exit 1
    }
    HTTP_BODY=""
    printf 'organization tailnets: %s (entries suppressed)\n' "$count"
    ;;
  probe-org-tailnet)
    [[ $# -eq 1 && "$1" == '--confirm' ]] || {
      printf 'error: probe-org-tailnet requires the exact --confirm flag\n' >&2
      usage
    }
    fetch_org_token
    PROBE_NAME="spurfire-probe-$(date +%s)-$$"
    [[ "$PROBE_NAME" == spurfire-probe-* && ${#PROBE_NAME} -le 50 ]] || {
      printf 'error: generated probe name failed safety policy\n' >&2
      exit 1
    }

    # Install cleanup before the create request. The trap is intentionally armed while child
    # fields are still empty, then gains enough information to delete as soon as parsing succeeds.
    trap cleanup_on_exit EXIT
    trap 'exit 130' INT TERM
    body="$(python3 - "$PROBE_NAME" <<'PY'
import json, sys
print(json.dumps({"displayName": sys.argv[1]}, separators=(",", ":")), end="")
PY
)"
    # From this point until a definitive non-success response, creation is potentially live.
    CREATED=true
    api_capture POST '/organizations/-/tailnets' "$body"
    body=""
    if [[ ! "$HTTP_STATUS" =~ ^2 ]]; then
      CREATED=false
      printf 'error: guarded create failed (HTTP %s; body discarded)\n' "$HTTP_STATUS" >&2
      HTTP_BODY=""
      exit 1
    fi
    fields="$(printf '%s' "$HTTP_BODY" | python3 -c '
import json, sys
value = json.load(sys.stdin)
fields = (
    value.get("oauthClient", {}).get("id"),
    value.get("oauthClient", {}).get("secret"),
    value.get("dnsName"),
    value.get("id"),
)
if not all(isinstance(item, str) and item and "\t" not in item for item in fields):
    raise SystemExit(1)
print("\t".join(fields), end="")
')" || {
      HTTP_BODY=""
      printf 'error: create response lacked typed cleanup fields; manual remediation may be required\n' >&2
      exit 1
    }
    HTTP_BODY=""
    IFS=$'\t' read -r CHILD_ID CHILD_SECRET CHILD_DNS CHILD_STABLE_ID <<<"$fields"
    fields=""
    printf 'created guarded probe %s (identifiers and child credentials redacted)\n' "$PROBE_NAME"

    fetch_child_token
    cleanup_created
    list_tailnets_capture
    if ! printf '%s' "$HTTP_BODY" | python3 -c '
import json, sys
stable_id = sys.argv[1]
tailnets = json.load(sys.stdin).get("tailnets", [])
raise SystemExit(1 if any(item.get("id") == stable_id for item in tailnets) else 0)
' "$CHILD_STABLE_ID"; then
      HTTP_BODY=""
      printf 'error: deleted probe stable ID remains in organization listing\n' >&2
      exit 1
    fi
    HTTP_BODY=""
    CHILD_ID=""
    CHILD_SECRET=""
    CHILD_TOKEN=""
    printf 'verified cleanup: stable ID absent; listing entries suppressed\n'
    ;;
esac
