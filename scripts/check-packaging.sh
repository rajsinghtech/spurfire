#!/usr/bin/env bash
# Credential-free Docker/Helm packaging contract checks. This script never pushes.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
chart="$repo_root/charts/spurfire-control"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/spurfire-packaging.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

command -v helm >/dev/null 2>&1 || {
  echo "error: helm is required" >&2
  exit 1
}

helm lint --strict "$chart"
helm template validate "$chart" > "$tmp/default.yaml"
helm template validate "$chart" \
  --set fullnameOverride=spurfire \
  --set config.dryRun=false \
  --set config.provisioningMode=tailnet_per_lobby \
  --set tailscale.existingSecret=fixture-parent-oauth \
  --set persistence.enabled=true \
  --set persistence.storageClass=standard-rwo \
  > "$tmp/staged.yaml"
helm template validate "$chart" \
  --set fullnameOverride=spurfire \
  --set httpRoute.enabled=true \
  > "$tmp/public-dry-run.yaml"
helm template validate "$chart" \
  --set image.digest=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  > "$tmp/digest.yaml"
helm package "$chart" --destination "$tmp" >/dev/null

grep -q 'replicas: 1' "$tmp/default.yaml"
grep -A1 -q 'strategy:.*$' "$tmp/default.yaml"
grep -q 'type: Recreate' "$tmp/default.yaml"
grep -q 'runAsNonRoot: true' "$tmp/default.yaml"
grep -q 'readOnlyRootFilesystem: true' "$tmp/default.yaml"
grep -q 'automountServiceAccountToken: false' "$tmp/default.yaml"
grep -q 'SPURFIRE_DRY_RUN: "1"' "$tmp/default.yaml"
grep -q 'SPURFIRE_REAL_MUTATIONS_ENABLED: "0"' "$tmp/default.yaml"
if grep -q 'SPURFIRE_MAX_ACTIVE_REAL_LOBBIES\|SPURFIRE_NETWORK_' "$tmp/default.yaml"; then
  echo "error: chart rendered deferred runtime settings the server does not consume" >&2
  exit 1
fi
grep -q 'emptyDir: {}' "$tmp/default.yaml"
if grep -q '^kind: HTTPRoute$' "$tmp/default.yaml"; then
  echo "error: public HTTPRoute must be opt-in" >&2
  exit 1
fi
if grep -q 'TS_CLIENT_ID\|TS_CLIENT_SECRET\|fixture-parent-oauth' "$tmp/default.yaml"; then
  echo "error: dry-run manifest unexpectedly references OAuth material" >&2
  exit 1
fi
if grep -q '^kind: Secret$' "$tmp/default.yaml" "$tmp/staged.yaml" "$tmp/public-dry-run.yaml"; then
  echo "error: chart must not create credential Secrets" >&2
  exit 1
fi

grep -q '^kind: PersistentVolumeClaim$' "$tmp/staged.yaml"
grep -q 'helm.sh/resource-policy: keep' "$tmp/staged.yaml"
grep -q 'SPURFIRE_REAL_MUTATIONS_ENABLED: "0"' "$tmp/staged.yaml"
if grep -q 'SPURFIRE_MAX_ACTIVE_REAL_LOBBIES\|SPURFIRE_NETWORK_' "$tmp/staged.yaml"; then
  echo "error: staged chart rendered deferred runtime settings" >&2
  exit 1
fi
grep -q 'name: TS_CLIENT_ID' "$tmp/staged.yaml"
grep -q 'name: TS_CLIENT_SECRET' "$tmp/staged.yaml"
grep -q 'name: "fixture-parent-oauth"' "$tmp/staged.yaml"
if grep -q '^kind: HTTPRoute$' "$tmp/staged.yaml"; then
  echo "error: activation-closed provider staging must not be public" >&2
  exit 1
fi

grep -q '^kind: HTTPRoute$' "$tmp/public-dry-run.yaml"
grep -q 'spurfire.rajsingh.info' "$tmp/public-dry-run.yaml"
grep -q 'name: public-gateway' "$tmp/public-dry-run.yaml"
grep -q 'namespace: gateway-system' "$tmp/public-dry-run.yaml"
grep -q 'SPURFIRE_DRY_RUN: "1"' "$tmp/public-dry-run.yaml"
grep -q 'SPURFIRE_REAL_MUTATIONS_ENABLED: "0"' "$tmp/public-dry-run.yaml"
if grep -q 'TS_CLIENT_ID\|TS_CLIENT_SECRET' "$tmp/public-dry-run.yaml"; then
  echo "error: public dry-run manifest unexpectedly references OAuth keys" >&2
  exit 1
fi
grep -q 'ghcr.io/rajsinghtech/spurfire-server@sha256:' "$tmp/digest.yaml"

expect_failure() {
  local name="$1"
  shift
  if helm template invalid "$chart" "$@" >"$tmp/$name.out" 2>"$tmp/$name.err"; then
    echo "error: unsafe values unexpectedly rendered ($name)" >&2
    exit 1
  fi
}

expect_failure real-mutations-activation-closed \
  --set config.dryRun=false \
  --set config.realMutationsEnabled=true \
  --set config.provisioningMode=tailnet_per_lobby \
  --set tailscale.existingSecret=fixture-parent-oauth \
  --set persistence.enabled=true
expect_failure real-lobby-quota-drift \
  --set config.maxActiveRealLobbies=2
expect_failure device-freshness-contract-drift \
  --set networkSummary.deviceInventory.freshForSeconds=15
expect_failure report-retention-contract-drift \
  --set networkSummary.participantReports.retentionSeconds=30
expect_failure dry-run-mode-mismatch \
  --set config.dryRun=true \
  --set config.provisioningMode=shared_tailnet
expect_failure dry-run-secret-reference \
  --set tailscale.existingSecret=fixture-parent-oauth
expect_failure real-dry-run-mode \
  --set config.dryRun=false \
  --set config.provisioningMode=dry_run \
  --set tailscale.existingSecret=spurfire-tailscale \
  --set persistence.enabled=true
expect_failure missing-oauth-secret \
  --set config.dryRun=false \
  --set config.provisioningMode=tailnet_per_lobby \
  --set persistence.enabled=true
expect_failure missing-real-persistence \
  --set config.dryRun=false \
  --set config.provisioningMode=tailnet_per_lobby \
  --set tailscale.existingSecret=fixture-parent-oauth
expect_failure public-non-dry-run \
  --set config.dryRun=false \
  --set config.provisioningMode=tailnet_per_lobby \
  --set tailscale.existingSecret=fixture-parent-oauth \
  --set persistence.enabled=true \
  --set httpRoute.enabled=true
expect_failure missing-route-host \
  --set httpRoute.enabled=true \
  --set-json 'httpRoute.hostnames=[]'
expect_failure reserved-pod-label \
  --set-string 'podLabels.app\.kubernetes\.io/name=bad'
expect_failure reserved-config-checksum \
  --set-string 'podAnnotations.checksum/config=bad'
expect_failure reserved-pvc-policy \
  --set-string 'persistence.annotations.helm\.sh/resource-policy=delete'

if find "$repo_root" -maxdepth 3 \( -name '*.tgz' -o -name '*.prov' \) -print -quit | grep -q .; then
  echo "error: generated Helm packages must not be committed" >&2
  exit 1
fi

echo "packaging contract checks passed"
