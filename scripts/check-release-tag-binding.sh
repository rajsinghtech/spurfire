#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <version> <tag-commit>" >&2
  exit 2
fi
version="$1"
tag_commit="$2"
evidence="docs/release-evidence/${version}.json"
notes="docs/release-notes-${version}.md"

test -s "$evidence"
test -s "$notes"
source_sha="$(python3 - "$evidence" <<'PY'
import json, sys
with open(sys.argv[1], encoding='utf-8') as handle:
    value = json.load(handle).get('source_sha', '')
if not isinstance(value, str):
    raise SystemExit(1)
print(value)
PY
)"
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]]
tag_sha="$(git rev-parse "${tag_commit}^{commit}")"
git cat-file -e "${source_sha}^{commit}"
git merge-base --is-ancestor "$source_sha" "$tag_sha"

while IFS= read -r changed; do
  case "$changed" in
    "$evidence"|"$notes") ;;
    *)
      echo "tag metadata commit changes non-release path: $changed" >&2
      exit 1
      ;;
  esac
done < <(git diff --name-only "$source_sha..$tag_sha")

printf '%s\n' "$source_sha"
