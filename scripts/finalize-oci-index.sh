#!/usr/bin/env bash
# Build and publish an OCI index without reading newly updated mutable tags.
set -euo pipefail

usage() {
  echo "usage: $0 IMAGE AMD64_DIGEST ARM64_DIGEST TAG [TAG ...]" >&2
  exit 2
}

(( $# >= 4 )) || usage
image="$1"
amd64_digest="$2"
arm64_digest="$3"
shift 3
tags=("$@")

[[ "$image" =~ ^ghcr\.io/[a-z0-9]+([._-][a-z0-9]+)*/[a-z0-9]+([._/-][a-z0-9]+)*$ ]] || {
  echo "invalid GHCR image name: $image" >&2
  exit 1
}
for digest in "$amd64_digest" "$arm64_digest"; do
  [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    echo "invalid native digest: $digest" >&2
    exit 1
  }
done
[[ "$amd64_digest" != "$arm64_digest" ]] || {
  echo "native digests must be distinct" >&2
  exit 1
}

tag_args=()
for tag in "${tags[@]}"; do
  [[ "$tag" == "$image:"* ]] || {
    echo "invalid or foreign image tag: $tag" >&2
    exit 1
  }
  tag_name="${tag#"$image:"}"
  [[ "$tag_name" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]] || {
    echo "invalid or foreign image tag: $tag" >&2
    exit 1
  }
  tag_args+=(--tag "$tag")
done

refs=("$image@$amd64_digest" "$image@$arm64_digest")
# Command substitution deliberately removes buildx's trailing newline. The registry
# hashes exactly these bytes; do not parse and re-serialize this JSON.
index_json="$(docker buildx imagetools create --dry-run "${refs[@]}")"

jq -e '
  . as $index |
  .schemaVersion == 2 and
  .mediaType == "application/vnd.oci.image.index.v1+json" and
  (.manifests | length) == 4 and
  ([.manifests[].digest] | length == (unique | length)) and
  (all(.manifests[];
    .mediaType == "application/vnd.oci.image.manifest.v1+json" and
    (.digest | test("^sha256:[0-9a-f]{64}$")))) and
  ([.manifests[] |
    select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest") |
    {os: .platform.os, architecture: .platform.architecture}] |
    sort_by(.architecture)) == [
      {os: "linux", architecture: "amd64"},
      {os: "linux", architecture: "arm64"}
    ] and
  ([.manifests[] |
    select(.annotations["vnd.docker.reference.type"] == "attestation-manifest")] |
    length) == 2 and
  (all(.manifests[] |
    select(.annotations["vnd.docker.reference.type"] == "attestation-manifest");
    .platform.os == "unknown" and
    .platform.architecture == "unknown" and
    (.annotations["vnd.docker.reference.digest"] as $subject |
      ([$index.manifests[] |
        select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest") |
        .digest] | index($subject)) != null))) and
  (all(.manifests[] |
    select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest");
    .digest as $subject |
    ([$index.manifests[] |
      select(.annotations["vnd.docker.reference.type"] == "attestation-manifest" and
             .annotations["vnd.docker.reference.digest"] == $subject)] |
      length) == 1))
' <<< "$index_json" >/dev/null

if command -v sha256sum >/dev/null 2>&1; then
  final_digest="sha256:$(printf '%s' "$index_json" | sha256sum | awk '{print $1}')"
else
  final_digest="sha256:$(printf '%s' "$index_json" | shasum -a 256 | awk '{print $1}')"
fi
[[ "$final_digest" =~ ^sha256:[0-9a-f]{64}$ ]]

docker buildx imagetools create \
  "${tag_args[@]}" \
  "${refs[@]}" >&2

printf '%s\n' "$final_digest"
