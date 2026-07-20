#!/usr/bin/env python3
"""Fail closed unless a source-bound Alpha release evidence manifest is complete."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

REQUIRED_GATES = (
    "activation_approved",
    "apple_distribution_trust",
    "artifact_launch_smoke",
    "cleanup_exact_absence",
    "client_preflight",
    "credential_free_linux",
    "gameplay_regressions",
    "independent_release_approval",
    "managed_two_download_lifecycle",
    "natural_m2_playtest",
    "one_lobby_client_flow",
    "provenance_verified",
    "secret_canaries",
    "telemetry_metrics",
    "windows_distribution_trust",
)
REQUIRED_PLATFORMS = (
    "linux-arm64",
    "linux-x86_64",
    "macos-universal",
    "windows-x86_64",
)


class ManifestError(ValueError):
    pass


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def validate(document: dict[str, Any], *, version: str | None, source_sha: str | None) -> dict[str, Any]:
    if document.get("schema_version") != 1:
        raise ManifestError("schema_version must be 1")
    actual_version = str(document.get("version", ""))
    if not re.fullmatch(
        r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)",
        actual_version,
    ):
        raise ManifestError("version must be stable semantic version X.Y.Z")
    if version is not None and actual_version != version:
        raise ManifestError(f"manifest version {actual_version} does not match {version}")
    actual_sha = str(document.get("source_sha", ""))
    if not re.fullmatch(r"[0-9a-f]{40}", actual_sha):
        raise ManifestError("source_sha must be a full lowercase Git SHA")
    if source_sha is not None and actual_sha != source_sha:
        raise ManifestError(f"manifest source_sha {actual_sha} does not match {source_sha}")

    blockers = document.get("blockers")
    if blockers != []:
        raise ManifestError("release evidence must contain an empty blockers array")
    gates = document.get("gates")
    if not isinstance(gates, dict):
        raise ManifestError("gates must be an object")
    missing = sorted(set(REQUIRED_GATES) - set(gates))
    extra = sorted(set(gates) - set(REQUIRED_GATES))
    if missing or extra:
        raise ManifestError(f"gate keys mismatch; missing={missing}, extra={extra}")
    failed = sorted(name for name, value in gates.items() if value is not True)
    if failed:
        raise ManifestError(f"release gates are not green: {failed}")

    runs = document.get("runs")
    if not isinstance(runs, dict):
        raise ManifestError("runs must be an object")
    for name in ("ci", "client_preflight", "private_live_lifecycle"):
        run = runs.get(name)
        if not isinstance(run, dict):
            raise ManifestError(f"missing run binding: {name}")
        if not isinstance(run.get("id"), int) or run["id"] < 1:
            raise ManifestError(f"{name} run id must be positive")
        if run.get("head_sha") != actual_sha or run.get("conclusion") != "success":
            raise ManifestError(f"{name} run is not a successful exact-SHA binding")
    private_live = runs["private_live_lifecycle"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", str(private_live.get("repository", ""))):
        raise ManifestError("private-live run must identify its external repository")
    if not re.fullmatch(r"[^/]+(?:/[^/]+)*\.(?:yml|yaml)", str(private_live.get("workflow_path", ""))):
        raise ManifestError("private-live run must identify its workflow path")
    for field in ("evidence_artifact", "evidence_file"):
        value = str(private_live.get(field, ""))
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", value) or value in {".", ".."}:
            raise ManifestError(f"private-live {field} must be a safe file name")
    if not re.fullmatch(r"[0-9a-f]{64}", str(private_live.get("evidence_sha256", ""))):
        raise ManifestError("private-live evidence lacks SHA-256")

    artifacts = document.get("artifacts")
    if not isinstance(artifacts, list):
        raise ManifestError("artifacts must be an array")
    by_platform = {str(item.get("platform", "")): item for item in artifacts if isinstance(item, dict)}
    if sorted(by_platform) != sorted(REQUIRED_PLATFORMS) or len(artifacts) != len(REQUIRED_PLATFORMS):
        raise ManifestError(
            f"artifacts must contain exactly the {len(REQUIRED_PLATFORMS)} supported platforms"
        )
    for platform in REQUIRED_PLATFORMS:
        item = by_platform[platform]
        if not re.fullmatch(r"[0-9a-f]{64}", str(item.get("sha256", ""))):
            raise ManifestError(f"{platform} artifact lacks SHA-256")
        for flag in ("provenance_verified", "sbom_present", "launch_smoke_passed"):
            if item.get(flag) is not True:
                raise ManifestError(f"{platform} artifact has not passed {flag}")

    trust = document.get("distribution_trust")
    if not isinstance(trust, dict):
        raise ManifestError("distribution_trust must be an object")
    mac = trust.get("macos", {})
    windows = trust.get("windows", {})
    if mac.get("developer_id_signed") is not True or mac.get("notarized") is not True:
        raise ManifestError("Apple Developer ID signing and notarization are mandatory")
    if windows.get("authenticode_signed") is not True:
        raise ManifestError("Windows Authenticode signing is mandatory")

    approvals = document.get("approvals")
    if not isinstance(approvals, dict):
        raise ManifestError("approvals must be an object")
    reviewers: set[str] = set()
    for name in ("activation", "release"):
        approval = approvals.get(name)
        if not isinstance(approval, dict) or approval.get("approved") is not True:
            raise ManifestError(f"missing independent {name} approval")
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", str(approval.get("evidence_digest", ""))):
            raise ManifestError(f"{name} approval lacks a SHA-256 evidence digest")
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", str(approval.get("repository", ""))):
            raise ManifestError(f"{name} approval must identify its GitHub repository")
        if not isinstance(approval.get("pull_request"), int) or approval["pull_request"] < 1:
            raise ManifestError(f"{name} approval pull request must be positive")
        if not isinstance(approval.get("review_id"), int) or approval["review_id"] < 1:
            raise ManifestError(f"{name} approval review id must be positive")
        reviewer = str(approval.get("reviewer", ""))
        if not re.fullmatch(r"[A-Za-z0-9-]+", reviewer):
            raise ManifestError(f"{name} approval must identify its reviewer")
        reviewers.add(reviewer.casefold())
    if len(reviewers) != 2:
        raise ManifestError("activation and release approvals require distinct reviewers")

    return {
        "ok": True,
        "version": actual_version,
        "source_sha": actual_sha,
        "artifact_count": len(artifacts),
        "gate_count": len(gates),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--version")
    parser.add_argument("--source-sha")
    parser.add_argument("--expected-sha256")
    args = parser.parse_args(argv)
    try:
        if args.expected_sha256:
            if not re.fullmatch(r"[0-9a-f]{64}", args.expected_sha256):
                raise ManifestError("expected manifest SHA-256 must be 64 lowercase hex characters")
            actual_digest = file_sha256(args.manifest)
            if actual_digest != args.expected_sha256:
                raise ManifestError("manifest SHA-256 does not match independent approval input")
        document = json.loads(args.manifest.read_text(encoding="utf-8"))
        if not isinstance(document, dict):
            raise ManifestError("manifest root must be an object")
        result = validate(document, version=args.version, source_sha=args.source_sha)
    except (OSError, json.JSONDecodeError, ManifestError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
