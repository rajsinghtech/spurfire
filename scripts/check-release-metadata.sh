#!/usr/bin/env bash
# Validate the version contract shared by the server, client, chart, lockfile, and release notes.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

manifest_version() {
  awk '
    /^\[package\]$/ { package = 1; next }
    package && /^\[/ { exit }
    package && /^version = / {
      gsub(/[" ]/, "", $3)
      print $3
      exit
    }
  ' "$1"
}

lock_version() {
  local package_name="$1"
  awk -v package_name="$package_name" '
    /^\[\[package\]\]$/ { package = 0; next }
    $0 == "name = \"" package_name "\"" { package = 1; next }
    package && /^version = / {
      gsub(/[" ]/, "", $3)
      print $3
      exit
    }
  ' Cargo.lock
}

require_equal() {
  local label="$1" actual="$2" expected="$3"
  if [[ -z "$actual" || "$actual" != "$expected" ]]; then
    printf 'error: %s is %q; expected %q\n' "$label" "$actual" "$expected" >&2
    exit 1
  fi
}

version="$(manifest_version crates/spurfire-server/Cargo.toml)"
semver_regex='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
[[ "$version" =~ $semver_regex ]] || {
  echo "error: invalid spurfire-server release version: $version" >&2
  exit 1
}

if [[ $# -gt 1 ]]; then
  echo "usage: scripts/check-release-metadata.sh [expected-version]" >&2
  exit 2
fi
if [[ $# -eq 1 ]]; then
  require_equal "requested release version" "$version" "$1"
fi

require_equal "Cargo.lock spurfire-server version" "$(lock_version spurfire-server)" "$version"

chart_version="$(awk '$1 == "version:" { gsub(/"/, "", $2); print $2; exit }' charts/spurfire-control/Chart.yaml)"
chart_app_version="$(awk '$1 == "appVersion:" { gsub(/"/, "", $2); print $2; exit }' charts/spurfire-control/Chart.yaml)"
require_equal "Helm chart version" "$chart_version" "$version"
require_equal "Helm chart appVersion" "$chart_app_version" "$version"

game_version="$(awk -F= '$1 == "config/version" { gsub(/"/, "", $2); print $2; exit }' game/project.godot)"
require_equal "Godot config/version" "$game_version" "$version"

for crate in cli control gdext net protocol; do
  package="spurfire-$crate"
  require_equal "$package manifest version" "$(manifest_version "crates/spurfire-$crate/Cargo.toml")" "0.1.0"
  require_equal "$package lockfile version" "$(lock_version "$package")" "0.1.0"
done

notes="docs/release-notes-${version}.md"
[[ -s "$notes" ]] || {
  echo "error: missing release notes: $notes" >&2
  exit 1
}
require_equal "release-note heading" "$(sed -n '1p' "$notes")" "# Spurfire $version"
grep -Fqx '## Playtest status' "$notes" || {
  echo "error: $notes must contain an explicit Playtest status section" >&2
  exit 1
}
grep -Fqi 'playtest pending' "$notes" || {
  echo "error: $notes must say that observational playtesting is pending" >&2
  exit 1
}
grep -Fq '| M2 | Saddle Dive | **implementation complete / playtest pending** |' docs/prototype-plan.md || {
  echo "error: docs/prototype-plan.md must keep M2 implementation complete / playtest pending" >&2
  exit 1
}

grep -Fq "Release candidate v${version}" crates/spurfire-server/src/landing.html || {
  echo "error: landing page source version does not match $version" >&2
  exit 1
}
grep -Fq 'Open latest published release' crates/spurfire-server/src/landing.html || {
  echo "error: landing page must not claim that the release candidate is already published" >&2
  exit 1
}

grep -Fq "github.ref == 'refs/heads/main'" .github/workflows/packages.yml || {
  echo "error: package publication must be limited to main/SHA artifacts, never tag aliases" >&2
  exit 1
}
grep -Fq 'scripts/check-alpha-evidence.py' .github/workflows/packages.yml || {
  echo "error: tag validation must require exact-SHA Alpha evidence" >&2
  exit 1
}
grep -Fq 'environment: alpha-release' .github/workflows/client-publish.yml || {
  echo "error: client publication must use the protected Alpha release environment" >&2
  exit 1
}
grep -Fq 'refusing to overwrite it' .github/workflows/client-publish.yml || {
  echo "error: client publication must refuse release overwrite" >&2
  exit 1
}
if grep -Eq '(^|[[:space:]])(git[[:space:]]+tag|git[[:space:]]+push|gh[[:space:]]+release[[:space:]]+create)' \
  .github/workflows/client-release.yml; then
  echo "error: Client Preflight must remain nonpublishing and must not create tags" >&2
  exit 1
fi

printf '%s\n' "$version"
