#!/usr/bin/env python3
"""Create deterministic checksums, SPDX metadata, and a nonpublishing candidate manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

EXPECTED = {
    "Spurfire-linux-x86_64.tar.gz": "linux-x86_64",
    "Spurfire-macos-universal.zip": "macos-universal",
    "Spurfire-windows-x86_64.zip": "windows-x86_64",
}


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--event", required=True)
    parser.add_argument("--provenance-verified", action="store_true")
    args = parser.parse_args(argv)

    if not re.fullmatch(r"[0-9a-f]{40}", args.source_sha):
        parser.error("--source-sha must be a full lowercase Git SHA")
    if args.run_id < 1 or args.run_attempt < 1:
        parser.error("run id and attempt must be positive")

    files = sorted(path for path in args.input_dir.iterdir() if path.is_file() and path.name in EXPECTED)
    if [path.name for path in files] != sorted(EXPECTED):
        print("error: candidate input must contain exactly the three expected client archives", file=sys.stderr)
        return 1
    args.output_dir.mkdir(parents=True, exist_ok=True)
    artifacts = []
    for path in files:
        artifacts.append(
            {
                "file": path.name,
                "platform": EXPECTED[path.name],
                "sha256": digest(path),
                "size_bytes": path.stat().st_size,
            }
        )

    checksums = "".join(f"{item['sha256']}  {item['file']}\n" for item in artifacts)
    (args.output_dir / "SHA256SUMS").write_text(checksums, encoding="utf-8")

    blockers = [
        "Apple Developer ID signing is absent",
        "Apple notarization is absent",
        "Windows Authenticode signing is absent",
        "one-lobby flow must pass the exact-SHA CI gate",
        "managed private-live lifecycle evidence is not part of a candidate build",
        "human feel and natural-play evidence requires separate qualification",
    ]
    if not args.provenance_verified:
        blockers.append("GitHub build-provenance attestations were not verified for this run")
    manifest = {
        "schema_version": 1,
        "candidate_only": True,
        "release_eligible": False,
        "source_sha": args.source_sha,
        "workflow": {
            "run_id": args.run_id,
            "run_attempt": args.run_attempt,
            "event": args.event,
        },
        "artifacts": artifacts,
        "provenance_verified": args.provenance_verified,
        "distribution_trust": {
            "macos": {"signature": "ad_hoc", "developer_id_signed": False, "notarized": False},
            "windows": {"signature": "unsigned", "authenticode_signed": False},
            "linux": {"signature": "unsigned_archive", "checksum_present": True},
        },
        "blockers": blockers,
        "publication": "forbidden",
    }
    (args.output_dir / "candidate-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    packages = []
    relationships = []
    for index, item in enumerate(artifacts, 1):
        spdx_id = f"SPDXRef-Package-{index}"
        packages.append(
            {
                "SPDXID": spdx_id,
                "name": item["file"],
                "versionInfo": args.source_sha,
                "downloadLocation": "NOASSERTION",
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": "BSD-3-Clause",
                "checksums": [{"algorithm": "SHA256", "checksumValue": item["sha256"]}],
                "externalRefs": [],
            }
        )
        relationships.append(
            {"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES", "relatedSpdxElement": spdx_id}
        )
    namespace = f"https://github.com/rajsinghtech/spurfire/candidate/{args.source_sha}/{args.run_id}-{args.run_attempt}"
    spdx = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"Spurfire Alpha candidate {args.source_sha}",
        "documentNamespace": namespace,
        "creationInfo": {"creators": ["Tool: scripts/make-alpha-candidate-metadata.py"], "created": "1970-01-01T00:00:00Z"},
        "packages": packages,
        "relationships": relationships,
    }
    (args.output_dir / "candidate.spdx.json").write_text(
        json.dumps(spdx, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
