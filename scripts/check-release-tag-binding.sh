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

tag_sha="$(git rev-parse "${tag_commit}^{commit}")"
# Evidence and notes are validated as committed at the tag so this check works
# from any checkout (for example a source-only candidate build checkout).
git cat-file -e "${tag_sha}:${evidence}"
git cat-file -e "${tag_sha}:${notes}"
source_sha="$(git show "${tag_sha}:${evidence}" | python3 -c '
import json, sys
value = json.load(sys.stdin).get("source_sha", "")
if not isinstance(value, str):
    raise SystemExit(1)
print(value)
')"
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]]
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
