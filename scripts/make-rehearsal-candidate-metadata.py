#!/usr/bin/env python3
"""Validate one exact-SHA, exact-origin, permanently nonpublishing rehearsal candidate."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

APPROVED_REHEARSAL_ORIGIN = "https://rehearsal.spurfire.rajsingh.info"
ARCHIVE = "Spurfire-macos-universal.zip"
TRUST_FIELDS = {
    "schema_version",
    "candidate_mode",
    "platform",
    "control_origin",
    "source_sha",
    "archive",
    "archive_sha256",
    "launch_smoke_passed",
    "signature",
    "signing_trust",
    "release_eligible",
    "publication",
}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--control-origin", required=True)
    args = parser.parse_args(argv)

    if not re.fullmatch(r"[0-9a-f]{40}", args.source_sha):
        parser.error("--source-sha must be a full lowercase Git SHA")
    if args.control_origin != APPROVED_REHEARSAL_ORIGIN:
        parser.error("--control-origin is not the compile-time reviewed rehearsal origin")

    archive = args.input_dir / ARCHIVE
    trust_path = args.input_dir / "macos-rehearsal-trust.json"
    if not archive.is_file() or not trust_path.is_file():
        return fail("the archive and rehearsal trust record are both required")
    try:
        trust = json.loads(trust_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return fail(f"invalid rehearsal trust record: {error}")
    if not isinstance(trust, dict) or set(trust) != TRUST_FIELDS:
        return fail("rehearsal trust record fields do not exactly match schema 2")

    expected = {
        "schema_version": 2,
        "candidate_mode": "rehearsal",
        "platform": "macos-universal",
        "control_origin": APPROVED_REHEARSAL_ORIGIN,
        "source_sha": args.source_sha,
        "archive": ARCHIVE,
        "archive_sha256": digest(archive),
        "launch_smoke_passed": True,
        "signature": "ad_hoc",
        "signing_trust": "untrusted",
        "release_eligible": False,
        "publication": "forbidden",
    }
    if trust != expected:
        return fail("rehearsal trust record does not exactly bind the candidate")

    manifest = {
        "schema_version": 2,
        "candidate_mode": "rehearsal",
        "candidate_only": True,
        "control_origin": APPROVED_REHEARSAL_ORIGIN,
        "source_sha": args.source_sha,
        "release_eligible": False,
        "publication": "forbidden",
        "artifacts": [
            {
                "platform": "macos-universal",
                "file": ARCHIVE,
                "sha256": expected["archive_sha256"],
            }
        ],
        "trust": trust,
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "rehearsal-candidate-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (args.output_dir / "SHA256SUMS").write_text(
        f"{expected['archive_sha256']}  {ARCHIVE}\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
