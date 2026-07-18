#!/usr/bin/env python3
"""Create deterministic checksums, SPDX metadata, and source-bound candidate metadata."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

EXPECTED = {
    "Spurfire-linux-x86_64.tar.gz": "linux-x86_64",
    "Spurfire-macos-universal.zip": "macos-universal",
    "Spurfire-windows-x86_64.zip": "windows-x86_64",
}
TRUST_FILES = {
    "linux-x86_64": "linux-trust.json",
    "macos-universal": "macos-trust.json",
    "windows-x86_64": "windows-trust.json",
}


class MetadataError(ValueError):
    pass


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def load_trust_record(
    input_dir: Path, platform: str, source_sha: str, artifact: dict[str, Any]
) -> dict[str, Any]:
    path = input_dir / TRUST_FILES[platform]
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise MetadataError(f"invalid {platform} trust record: {error}") from error
    if not isinstance(record, dict) or record.get("schema_version") != 1:
        raise MetadataError(f"{platform} trust record schema_version must be 1")
    expected = {
        "platform": platform,
        "source_sha": source_sha,
        "archive": artifact["file"],
        "archive_sha256": artifact["sha256"],
    }
    for field, value in expected.items():
        if record.get(field) != value:
            raise MetadataError(f"{platform} trust record {field} does not match the candidate")
    if record.get("launch_smoke_passed") is not True:
        raise MetadataError(f"{platform} final archive launch smoke did not pass")
    return record


def macos_trusted(record: dict[str, Any], approved_team_id: str | None) -> bool:
    verification = record.get("verification")
    return (
        approved_team_id is not None
        and record.get("signature") == "developer_id"
        and record.get("developer_id_signed") is True
        and record.get("notarized") is True
        and isinstance(verification, dict)
        and verification.get("codesign_deep_strict") is True
        and verification.get("notarization_stapled") is True
        and verification.get("gatekeeper_assessment") is True
        and verification.get("team_id") == approved_team_id
    )


def windows_trusted(
    record: dict[str, Any], approved_certificate_sha256: str | None
) -> bool:
    verification = record.get("verification")
    return (
        approved_certificate_sha256 is not None
        and record.get("signature") == "authenticode"
        and record.get("authenticode_signed") is True
        and isinstance(verification, dict)
        and verification.get("status") == "Valid"
        and verification.get("timestamp_verified") is True
        and verification.get("signer_certificate_sha256")
        == approved_certificate_sha256
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--event", required=True)
    parser.add_argument(
        "--candidate-mode",
        choices=("preflight", "trusted-release"),
        default="preflight",
    )
    parser.add_argument("--provenance-verified", action="store_true")
    parser.add_argument("--approved-apple-team-id")
    parser.add_argument("--approved-windows-certificate-sha256")
    args = parser.parse_args(argv)

    if not re.fullmatch(r"[0-9a-f]{40}", args.source_sha):
        parser.error("--source-sha must be a full lowercase Git SHA")
    if args.run_id < 1 or args.run_attempt < 1:
        parser.error("run id and attempt must be positive")
    if args.candidate_mode == "trusted-release" and args.event != "workflow_dispatch":
        parser.error("trusted-release mode is restricted to workflow_dispatch")
    if args.candidate_mode == "trusted-release":
        if not re.fullmatch(r"[A-Z0-9]{10}", args.approved_apple_team_id or ""):
            parser.error("trusted-release mode requires a 10-character approved Apple team ID")
        if not re.fullmatch(
            r"[0-9a-f]{64}", args.approved_windows_certificate_sha256 or ""
        ):
            parser.error(
                "trusted-release mode requires an approved Windows certificate SHA-256"
            )

    files = sorted(
        path
        for path in args.input_dir.iterdir()
        if path.is_file() and path.name in EXPECTED
    )
    if [path.name for path in files] != sorted(EXPECTED):
        print(
            "error: candidate input must contain exactly the three expected client archives",
            file=sys.stderr,
        )
        return 1
    args.output_dir.mkdir(parents=True, exist_ok=True)
    artifacts = [
        {
            "file": path.name,
            "platform": EXPECTED[path.name],
            "sha256": digest(path),
            "size_bytes": path.stat().st_size,
        }
        for path in files
    ]

    try:
        trust = {
            artifact["platform"]: load_trust_record(
                args.input_dir, artifact["platform"], args.source_sha, artifact
            )
            for artifact in artifacts
        }
    except MetadataError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    checksums = "".join(
        f"{item['sha256']}  {item['file']}\n" for item in artifacts
    )
    (args.output_dir / "SHA256SUMS").write_text(checksums, encoding="utf-8")

    mac_trusted = macos_trusted(
        trust["macos-universal"], args.approved_apple_team_id
    )
    win_trusted = windows_trusted(
        trust["windows-x86_64"], args.approved_windows_certificate_sha256
    )
    trusted_mode = args.candidate_mode == "trusted-release"
    release_eligible = trusted_mode and args.provenance_verified and mac_trusted and win_trusted
    if trusted_mode:
        blockers = []
        if not args.provenance_verified:
            blockers.append("source-bound GitHub build-provenance attestations were not verified")
        if not mac_trusted:
            blockers.append("verified Apple Developer ID signing and notarization are absent")
        if not win_trusted:
            blockers.append("verified Windows Authenticode signing and timestamp are absent")
    else:
        blockers = [
            "ordinary preflight candidates are never release eligible",
            "Apple Developer ID signing is absent from the ordinary preflight",
            "Apple notarization is absent from the ordinary preflight",
            "Windows Authenticode signing is absent from the ordinary preflight",
            "managed private-live and human evidence require separate qualification",
        ]
        if not args.provenance_verified:
            blockers.append("source-bound GitHub build-provenance attestations were not verified")

    manifest = {
        "schema_version": 2,
        "candidate_mode": args.candidate_mode,
        "candidate_only": not release_eligible,
        "release_eligible": release_eligible,
        "source_sha": args.source_sha,
        "workflow": {
            "run_id": args.run_id,
            "run_attempt": args.run_attempt,
            "event": args.event,
        },
        "artifacts": artifacts,
        "provenance_verified": args.provenance_verified,
        "distribution_trust": {
            "macos": {
                "signature": trust["macos-universal"].get("signature"),
                "developer_id_signed": mac_trusted,
                "notarized": mac_trusted,
            },
            "windows": {
                "signature": trust["windows-x86_64"].get("signature"),
                "authenticode_signed": win_trusted,
            },
            "linux": {
                "signature": trust["linux-x86_64"].get("signature"),
                "checksum_present": True,
            },
        },
        "blockers": blockers,
        "publication": "protected_manual_only" if release_eligible else "forbidden",
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
                "checksums": [
                    {"algorithm": "SHA256", "checksumValue": item["sha256"]}
                ],
                "externalRefs": [],
            }
        )
        relationships.append(
            {
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": spdx_id,
            }
        )
    namespace = (
        f"https://github.com/rajsinghtech/spurfire/candidate/"
        f"{args.source_sha}/{args.run_id}-{args.run_attempt}"
    )
    spdx = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"Spurfire Alpha candidate {args.source_sha}",
        "documentNamespace": namespace,
        "creationInfo": {
            "creators": ["Tool: scripts/make-alpha-candidate-metadata.py"],
            "created": "1970-01-01T00:00:00Z",
        },
        "packages": packages,
        "relationships": relationships,
    }
    (args.output_dir / "candidate.spdx.json").write_text(
        json.dumps(spdx, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
