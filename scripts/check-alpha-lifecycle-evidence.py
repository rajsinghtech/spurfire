#!/usr/bin/env python3
"""Validate redacted two-client and cleanup evidence without performing mutations."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

PROHIBITED_KEYS = (
    "authorization",
    "auth_key",
    "capability",
    "client_secret",
    "credential",
    "endpoint",
    "invitation",
    "join_code",
    "oauth",
    "private_key",
    "token",
)
PROHIBITED_VALUES = (
    re.compile(r"\bBearer\s+\S+", re.IGNORECASE),
    re.compile(r"\btskey-", re.IGNORECASE),
    re.compile(r"https?://", re.IGNORECASE),
    re.compile(r"\b100\.(?:6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])(?:\.\d{1,3}){2}\b"),
    re.compile(r"\bfd7a:115c:a1e0\b", re.IGNORECASE),
)
ROUTE_CLASSES = {"direct", "derp", "peer_relay", "unknown"}


class EvidenceError(ValueError):
    pass


def _scan(value: Any, path: str = "$") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            normalized = str(key).lower().replace("-", "_")
            if any(part in normalized for part in PROHIBITED_KEYS):
                raise EvidenceError(f"prohibited field at {path}.{key}")
            _scan(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _scan(child, f"{path}[{index}]")
    elif isinstance(value, str):
        if any(pattern.search(value) for pattern in PROHIBITED_VALUES):
            raise EvidenceError(f"prohibited value at {path}")


def _one(events: list[dict[str, Any]], name: str, **matches: Any) -> dict[str, Any]:
    found = [
        event
        for event in events
        if event.get("event") == name and all(event.get(key) == value for key, value in matches.items())
    ]
    if len(found) != 1:
        suffix = " " + " ".join(f"{key}={value}" for key, value in matches.items()) if matches else ""
        raise EvidenceError(f"expected exactly one {name}{suffix}; found {len(found)}")
    return found[0]


def validate(document: dict[str, Any], *, require_live: bool) -> dict[str, Any]:
    _scan(document)
    if document.get("schema_version") != 1:
        raise EvidenceError("schema_version must be 1")
    source_sha = str(document.get("source_sha", ""))
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise EvidenceError("source_sha must be a full lowercase Git SHA")
    mode = document.get("mode")
    if mode not in {"simulated", "private_live"}:
        raise EvidenceError("mode must be simulated or private_live")
    if require_live and mode != "private_live":
        raise EvidenceError("release evidence requires private_live mode")
    archives = document.get("candidate_archives")
    if not isinstance(archives, dict) or sorted(archives) != ["a", "b"]:
        raise EvidenceError("candidate_archives must contain exactly a and b")
    for client, digest in archives.items():
        if not re.fullmatch(r"[0-9a-f]{64}", str(digest)):
            raise EvidenceError(f"candidate archive {client} lacks a SHA-256 digest")
    events = document.get("events")
    if not isinstance(events, list) or not all(isinstance(event, dict) for event in events):
        raise EvidenceError("events must be an array of objects")

    for client in ("a", "b"):
        _one(events, "download_verified", client=client)
        _one(events, "joined", client=client)
        _one(events, "m2_coherent", client=client)
        _one(events, "leave_confirmed", client=client)

    rosters = [_one(events, "roster_observed", client=client) for client in ("a", "b")]
    roster_projection = {
        (
            str(row.get("roster_hash", "")),
            int(row.get("network_generation", 0)),
            int(row.get("session_generation", 0)),
            int(row.get("roster_revision", 0)),
            tuple(sorted(str(actor) for actor in row.get("actors", []))),
        )
        for row in rosters
    }
    if len(roster_projection) != 1:
        raise EvidenceError("client roster/generation projections do not match exactly")
    roster = next(iter(roster_projection))
    if not re.fullmatch(r"[0-9a-f]{64}", roster[0]) or min(roster[1:4]) < 1 or len(roster[4]) != 2:
        raise EvidenceError("roster evidence is incomplete")

    for client in ("a", "b"):
        health = _one(events, "network_health", client=client)
        route = str(health.get("route_class", "")).lower()
        if route not in ROUTE_CLASSES:
            raise EvidenceError(f"invalid route_class for client {client}")
        rtt = health.get("rtt_ms")
        if route == "unknown":
            if rtt is not None:
                raise EvidenceError("unknown network health must not fabricate RTT")
        elif isinstance(rtt, bool) or not isinstance(rtt, (int, float)) or float(rtt) <= 0:
            raise EvidenceError("measured network health requires positive RTT")

    control_membership = _one(events, "control_service_membership")
    if control_membership.get("member") is not False:
        raise EvidenceError("control service must not join the gameplay tailnet")

    if mode == "simulated":
        _one(events, "simulated_cleanup_complete")
    else:
        observations = sorted(
            [event for event in events if event.get("event") == "exact_absence_observed"],
            key=lambda event: int(event.get("observation", 0)),
        )
        if len(observations) != 2 or [event.get("observation") for event in observations] != [1, 2]:
            raise EvidenceError("private_live cleanup requires exactly two ordered absence observations")
        digest = str(observations[0].get("stable_id_digest", ""))
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
            raise EvidenceError("absence evidence needs a redacted stable-ID digest")
        if observations[1].get("stable_id_digest") != digest:
            raise EvidenceError("absence observations refer to different stable IDs")
        first_ms = int(observations[0].get("completed_ms", -1))
        second_ms = int(observations[1].get("completed_ms", -1))
        if first_ms < 0 or second_ms - first_ms < 5000:
            raise EvidenceError("absence observations must complete at least five seconds apart")
        if any(event.get("exact_id_present") is not False for event in observations):
            raise EvidenceError("exact stable ID was present in an absence observation")
        vault = _one(events, "vault_erasure_verified")
        absent = _one(events, "dedicated_absent")
        if int(vault.get("completed_ms", -1)) < second_ms:
            raise EvidenceError("vault erasure occurred before exact absence proof")
        if int(absent.get("completed_ms", -1)) < int(vault.get("completed_ms", -1)):
            raise EvidenceError("DEDICATED_ABSENT preceded verified vault erasure")
        if absent.get("lease_released") is not True:
            raise EvidenceError("DEDICATED_ABSENT did not atomically release the lease")

    return {
        "ok": True,
        "mode": mode,
        "source_sha": source_sha,
        "event_count": len(events),
        "roster_hash": roster[0],
        "release_qualifying": mode == "private_live",
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--require-live", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    try:
        document = json.loads(args.evidence.read_text(encoding="utf-8"))
        if not isinstance(document, dict):
            raise EvidenceError("evidence root must be an object")
        result = validate(document, require_live=args.require_live)
    except (OSError, json.JSONDecodeError, EvidenceError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
