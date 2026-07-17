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
  --set tailscale.existingSecret=spurfire-tailscale \
  --set persistence.enabled=true \
  --set persistence.storageClass=ceph-block-replicated \
  --set httpRoute.enabled=true \
  > "$tmp/real.yaml"
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
grep -q 'emptyDir: {}' "$tmp/default.yaml"
if grep -q '^kind: HTTPRoute$' "$tmp/default.yaml"; then
  echo "error: public HTTPRoute must be opt-in" >&2
  exit 1
fi
if grep -q 'TS_CLIENT_ID\|TS_CLIENT_SECRET' "$tmp/default.yaml"; then
  echo "error: dry-run manifest unexpectedly references OAuth keys" >&2
  exit 1
fi

grep -q '^kind: PersistentVolumeClaim$' "$tmp/real.yaml"
grep -q 'helm.sh/resource-policy: keep' "$tmp/real.yaml"
grep -q 'name: TS_CLIENT_ID' "$tmp/real.yaml"
grep -q 'name: TS_CLIENT_SECRET' "$tmp/real.yaml"
grep -q '^kind: HTTPRoute$' "$tmp/real.yaml"
grep -q 'spurfire.rajsingh.info' "$tmp/real.yaml"
grep -q 'name: public' "$tmp/real.yaml"
grep -q 'namespace: home' "$tmp/real.yaml"
grep -q 'ghcr.io/rajsinghtech/spurfire-server@sha256:' "$tmp/digest.yaml"

expect_failure() {
  local name="$1"
  shift
  if helm template invalid "$chart" "$@" >"$tmp/$name.out" 2>"$tmp/$name.err"; then
    echo "error: unsafe values unexpectedly rendered ($name)" >&2
    exit 1
  fi
}

expect_failure dry-run-mode-mismatch \
  --set config.dryRun=true \
  --set config.provisioningMode=shared_tailnet
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
  --set tailscale.existingSecret=spurfire-tailscale
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
