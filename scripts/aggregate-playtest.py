#!/usr/bin/env python3
"""Deterministically aggregate Spurfire's secret-free Alpha playtest JSONL.

The recorder schema is intentionally append-only. Records may place event fields at the
root or under ``payload``; this tool normalizes both forms, rejects secret/topology fields,
and emits stable sorted JSON suitable for release evidence.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable

TICK_RATE = 60.0
NOTIFICATIONS = (
    "AIRBORNE REVERSAL",
    "FLYING DISMOUNT",
    "FULL-GALLOP HIT",
    "SADDLE DIVE HEADSHOT",
)
PROHIBITED_KEY_PARTS = (
    "authorization",
    "auth_key",
    "capability",
    "client_secret",
    "credential",
    "endpoint",
    "join_code",
    "oauth",
    "private_key",
    "seed",
    "token",
)
PROHIBITED_VALUE_PATTERNS = (
    re.compile(r"\bBearer\s+\S+", re.IGNORECASE),
    re.compile(r"\btskey-[A-Za-z0-9_-]+", re.IGNORECASE),
    re.compile(r"\b(?:100\.(?:6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7]))(?:\.\d{1,3}){2}\b"),
    re.compile(r"\bfd7a:115c:a1e0\b", re.IGNORECASE),
    re.compile(r"https?://", re.IGNORECASE),
)


class InputError(ValueError):
    """A playtest input violated the deterministic, secret-free contract."""


def _event_type(record: dict[str, Any]) -> str:
    return str(record.get("event_type", record.get("type", ""))).strip().lower()


def _flatten(record: dict[str, Any]) -> dict[str, Any]:
    payload = record.get("payload", {})
    if payload is None:
        payload = {}
    if not isinstance(payload, dict):
        raise InputError("payload must be an object")
    merged = dict(record)
    merged.pop("payload", None)
    merged.update(payload)
    return merged


def _scan_secret_free(value: Any, path: str = "$") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            normalized = str(key).lower().replace("-", "_")
            if any(part in normalized for part in PROHIBITED_KEY_PARTS):
                raise InputError(f"prohibited field at {path}.{key}")
            _scan_secret_free(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _scan_secret_free(child, f"{path}[{index}]")
    elif isinstance(value, str):
        for pattern in PROHIBITED_VALUE_PATTERNS:
            if pattern.search(value):
                raise InputError(f"prohibited value at {path}")


def _number(value: Any, label: str, *, minimum: float | None = None) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(float(value)):
        raise InputError(f"{label} must be a finite number")
    number = float(value)
    if minimum is not None and number < minimum:
        raise InputError(f"{label} must be >= {minimum}")
    return number


def _integer(value: Any, label: str, *, minimum: int | None = None) -> int:
    number = _number(value, label, minimum=float(minimum) if minimum is not None else None)
    if not number.is_integer():
        raise InputError(f"{label} must be an integer")
    return int(number)


def _percentile(values: list[float], percentile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    rank = max(0, math.ceil(percentile * len(ordered)) - 1)
    return ordered[rank]


def _rounded(value: float | None) -> float | None:
    return None if value is None else round(value, 6)


def read_records(paths: Iterable[Path]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for path in sorted(paths, key=lambda item: str(item)):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as error:
            raise InputError(f"cannot read {path}: {error}") from error
        for line_number, line in enumerate(lines, 1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as error:
                raise InputError(f"{path}:{line_number}: invalid JSON: {error.msg}") from error
            if not isinstance(record, dict):
                raise InputError(f"{path}:{line_number}: record must be an object")
            _scan_secret_free(record)
            flattened = _flatten(record)
            flattened["_source"] = f"{path}:{line_number}"
            records.append(flattened)
    if not records:
        raise InputError("no playtest records found")
    return records


def aggregate(records: list[dict[str, Any]], *, strict: bool) -> dict[str, Any]:
    sessions: dict[str, dict[str, Any]] = {}
    actors_by_session: dict[str, set[str]] = defaultdict(set)
    starts: set[tuple[str, str, int]] = set()
    finalized: dict[tuple[str, str, int], dict[str, Any]] = {}
    gallop_shots = gallop_hits = 0
    notifications: Counter[str] = Counter()
    notifications_by_session: dict[str, set[str]] = defaultdict(set)
    reload_rejections: Counter[str] = Counter()
    render: dict[int, dict[str, Any]] = defaultdict(
        lambda: {"frame_deltas_ms": [], "linear_jerk": [], "angular_jerk": [], "repeated": 0}
    )
    warnings: list[str] = []
    build_commits: set[str] = set()

    for raw in records:
        record = raw
        source = str(record.get("_source", "input"))
        event = _event_type(record)
        if not event:
            raise InputError(f"{source}: missing event_type")
        schema = _integer(record.get("schema_version", 0), f"{source}: schema_version", minimum=1)
        if schema != 1:
            raise InputError(f"{source}: unsupported schema_version {schema}")
        session_id = str(record.get("session_id", "")).strip()
        if not session_id:
            raise InputError(f"{source}: missing session_id")
        build = str(record.get("build_commit", "")).lower()
        if not re.fullmatch(r"[0-9a-f]{40}", build):
            raise InputError(f"{source}: build_commit must be a full lowercase Git SHA")
        build_commits.add(build)
        session = sessions.setdefault(
            session_id,
            {"build_commit": build, "start_ms": None, "end_ms": None, "censored": False},
        )
        if session["build_commit"] != build:
            raise InputError(f"{source}: session {session_id} changed build_commit")
        actor = str(record.get("actor", "")).strip()
        if actor:
            actors_by_session[session_id].add(actor)

        if event == "session_started":
            timestamp = _integer(record.get("timestamp_ms"), f"{source}: timestamp_ms", minimum=0)
            if session["start_ms"] is not None:
                raise InputError(f"{source}: duplicate session_started")
            session["start_ms"] = timestamp
        elif event in {"session_ended", "session_censored"}:
            timestamp = _integer(record.get("timestamp_ms"), f"{source}: timestamp_ms", minimum=0)
            if session["end_ms"] is not None:
                raise InputError(f"{source}: duplicate session terminal record")
            session["end_ms"] = timestamp
            session["censored"] = event == "session_censored"
        elif event == "dive_started":
            if not actor:
                raise InputError(f"{source}: dive_started missing actor")
            dive_id = _integer(record.get("dive_id"), f"{source}: dive_id", minimum=1)
            key = (session_id, actor, dive_id)
            if key in starts:
                raise InputError(f"{source}: duplicate dive_started {key}")
            starts.add(key)
        elif event in {"dive_finalized", "dive_censored"}:
            if not actor:
                raise InputError(f"{source}: terminal dive missing actor")
            dive_id = _integer(record.get("dive_id"), f"{source}: dive_id", minimum=1)
            key = (session_id, actor, dive_id)
            if key in finalized:
                raise InputError(f"{source}: duplicate terminal dive {key}")
            row = dict(record)
            row["terminal_kind"] = event
            finalized[key] = row
        elif event == "combat_shot" and str(record.get("context", "")).lower() == "gallop":
            if bool(record.get("accepted", False)):
                gallop_shots += 1
                gallop_hits += int(bool(record.get("hit", False)))
        elif event == "notification":
            text = str(record.get("text", "")).upper()
            if text in NOTIFICATIONS:
                notifications[text] += 1
                notifications_by_session[session_id].add(text)
        elif event == "reload_rejected":
            reason = str(record.get("reason", "unknown")).strip().lower() or "unknown"
            reload_rejections[reason] += 1
        elif event == "render_sample":
            refresh_hz = _integer(record.get("refresh_hz"), f"{source}: refresh_hz", minimum=1)
            bucket = render[refresh_hz]
            bucket["frame_deltas_ms"].append(
                _number(record.get("frame_delta_ms"), f"{source}: frame_delta_ms", minimum=0.0)
            )
            bucket["linear_jerk"].append(
                _number(record.get("linear_jerk", 0.0), f"{source}: linear_jerk", minimum=0.0)
            )
            bucket["angular_jerk"].append(
                _number(record.get("angular_jerk", 0.0), f"{source}: angular_jerk", minimum=0.0)
            )
            bucket["repeated"] += int(bool(record.get("repeated_transform", False)))

    incomplete = sorted(starts - set(finalized))
    terminal_without_start = sorted(set(finalized) - starts)
    if incomplete:
        warnings.append(f"{len(incomplete)} accepted dives lack a terminal row")
    if terminal_without_start:
        warnings.append(f"{len(terminal_without_start)} terminal dives lack a start row")
    if strict and warnings:
        raise InputError("; ".join(warnings))

    total_player_ms = 0
    session_rows: list[dict[str, Any]] = []
    for session_id in sorted(sessions):
        session = sessions[session_id]
        start = session["start_ms"]
        end = session["end_ms"]
        if start is None or end is None or end < start:
            message = f"session {session_id} lacks a valid start/end pair"
            if strict:
                raise InputError(message)
            warnings.append(message)
            duration = 0
        else:
            duration = end - start
        actors = sorted(actors_by_session[session_id])
        total_player_ms += duration * len(actors)
        session_rows.append(
            {
                "session_id": session_id,
                "build_commit": session["build_commit"],
                "duration_ms": duration,
                "censored": bool(session["censored"]),
                "actors": actors,
            }
        )

    dive_rows = [finalized[key] for key in sorted(finalized)]
    airborne_shots = sum(_integer(row.get("shots_fired", 0), "shots_fired", minimum=0) for row in dive_rows)
    airborne_hits = sum(_integer(row.get("shots_hit", 0), "shots_hit", minimum=0) for row in dive_rows)
    deaths_known = [bool(row["death_within_3s"]) for row in dive_rows if row.get("death_within_3s") is not None]
    clamp_known = [bool(row.get("direction_was_clamped", False)) for row in dive_rows]
    remount_ticks = [
        _integer(row["time_to_remount_ticks"], "time_to_remount_ticks", minimum=0)
        for row in dive_rows
        if row.get("time_to_remount_ticks") is not None
    ]
    airborne_rate = airborne_hits / airborne_shots if airborne_shots else None
    gallop_rate = gallop_hits / gallop_shots if gallop_shots else None
    uplift = (
        ((airborne_rate / gallop_rate) - 1.0) * 100.0
        if airborne_rate is not None and gallop_rate not in (None, 0.0)
        else None
    )
    dives_per_15 = len(dive_rows) * 900_000.0 / total_player_ms if total_player_ms else None

    render_rows: list[dict[str, Any]] = []
    for refresh_hz in sorted(render):
        bucket = render[refresh_hz]
        count = len(bucket["frame_deltas_ms"])
        render_rows.append(
            {
                "refresh_hz": refresh_hz,
                "sample_count": count,
                "repeated_transform_count": bucket["repeated"],
                "frame_delta_ms_p50": _rounded(_percentile(bucket["frame_deltas_ms"], 0.50)),
                "frame_delta_ms_p95": _rounded(_percentile(bucket["frame_deltas_ms"], 0.95)),
                "linear_jerk_p95": _rounded(_percentile(bucket["linear_jerk"], 0.95)),
                "angular_jerk_p95": _rounded(_percentile(bucket["angular_jerk"], 0.95)),
            }
        )

    chronological_sessions = sorted(
        sessions,
        key=lambda session_id: (
            sessions[session_id]["start_ms"] if sessions[session_id]["start_ms"] is not None else math.inf,
            session_id,
        ),
    )
    first_three_notifications: set[str] = set()
    for session_id in chronological_sessions[:3]:
        first_three_notifications.update(notifications_by_session[session_id])

    return {
        "schema_version": 1,
        "build_commits": sorted(build_commits),
        "session_count": len(session_rows),
        "sessions": session_rows,
        "player_session_minutes": _rounded(total_player_ms / 60_000.0),
        "dive_metrics": {
            "terminal_dives": len(dive_rows),
            "finalized_dives": sum(row["terminal_kind"] == "dive_finalized" for row in dive_rows),
            "censored_dives": sum(row["terminal_kind"] == "dive_censored" for row in dive_rows),
            "dives_per_player_15_minutes": _rounded(dives_per_15),
            "airborne_shots": airborne_shots,
            "airborne_hits": airborne_hits,
            "airborne_hit_rate": _rounded(airborne_rate),
            "gallop_shots": gallop_shots,
            "gallop_hits": gallop_hits,
            "gallop_hit_rate": _rounded(gallop_rate),
            "airborne_hit_rate_uplift_percent": _rounded(uplift),
            "landing_death_known": len(deaths_known),
            "landing_death_rate": _rounded(sum(deaths_known) / len(deaths_known) if deaths_known else None),
            "clamp_share": _rounded(sum(clamp_known) / len(clamp_known) if clamp_known else None),
            "remount_seconds_median": _rounded(
                statistics.median(remount_ticks) / TICK_RATE if remount_ticks else None
            ),
        },
        "notification_counts": {name: notifications[name] for name in NOTIFICATIONS},
        "notification_coverage": sum(notifications[name] > 0 for name in NOTIFICATIONS),
        "notification_coverage_within_first_three_sessions": all(
            name in first_three_notifications for name in NOTIFICATIONS
        ),
        "reload_rejection_counts": dict(sorted(reload_rejections.items())),
        "render_metrics": render_rows,
        "integrity": {
            "accepted_dive_count": len(starts),
            "terminal_without_start": len(terminal_without_start),
            "accepted_without_terminal": len(incomplete),
            "warnings": sorted(set(warnings)),
        },
    }


def _expand_inputs(values: list[str]) -> list[Path]:
    paths: list[Path] = []
    for value in values:
        path = Path(value)
        if path.is_dir():
            paths.extend(sorted(path.glob("*.jsonl")))
        else:
            paths.append(path)
    return paths


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", help="JSONL files or directories")
    parser.add_argument("--output", type=Path, help="write JSON here instead of stdout")
    parser.add_argument("--strict", action="store_true", help="reject incomplete session/dive rows")
    args = parser.parse_args(argv)
    try:
        records = read_records(_expand_inputs(args.inputs))
        result = aggregate(records, strict=args.strict)
    except InputError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
