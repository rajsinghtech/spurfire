#!/usr/bin/env python3
"""Validate an exact multi-process Godot route/RTT/HUD evidence matrix."""

from __future__ import annotations

import re
import statistics
import sys
from collections import Counter
from pathlib import Path


class EvidenceError(RuntimeError):
    pass


PAIR = re.compile(
    r"SPURFIRE_GODOT_P2P_(MEASURED|HUD) local=(\w+) peer=(\w+) "
    r"route=(.+?) rtt_ms=(\d+)"
)
READY = re.compile(r"SPURFIRE_GODOT_P2P_READY local=(\w+) peers=(\d+)")
SNAPSHOT = re.compile(r"SPURFIRE_GODOT_P2P_SNAPSHOT local=(\w+) sender=([0-9a-f-]+)")
INPUT = re.compile(r"SPURFIRE_GODOT_P2P_INPUT local=(\w+) sender=([0-9a-f-]+)")
QUALIFIED = re.compile(r"SPURFIRE_GODOT_P2P_QUALIFIED local=(\w+) peers=(\d+) snapshots=(\d+)")
RTT_READY = re.compile(r"SPURFIRE_GODOT_P2P_RTT_READY local=(\w+) peer=(\w+) samples=(\d+)")
SOAK = re.compile(
    r"SPURFIRE_GODOT_P2P_SOAK local=(\w+) role=(authority|follower) "
    r"duration_ms=(\d+) snapshots=(\d+) inputs=(\d+) min_sender_inputs=(\d+) peak_gap_ms=(\d+) "
    r"motion_span_mm=(\d+) last_age_ms=(\d+) presentation_samples=(\d+) "
    r"presentation_desync_ms=(\d+) rejects=(\d+)"
)


def normalize_route(route: str) -> str:
    upper = route.upper().replace("_", " ")
    if "PEER" in upper and "RELAY" in upper:
        return "PEER RELAY"
    if "DERP" in upper:
        return "DERP"
    if "DIRECT" in upper:
        return "DIRECT"
    raise EvidenceError(f"unknown route class {route!r}")


def validate(log_dir: Path, nodes: list[str], minimum_soak_ms: int = 0) -> str:
    if len(nodes) < 2 or len(nodes) != len(set(nodes)):
        raise EvidenceError("node roster must contain distinct peers")
    player_to_node = {
        f"00000000-0000-4000-8000-{index + 2:012d}": node
        for index, node in enumerate("abcdefgh")
    }
    expected = {(local, peer) for local in nodes for peer in nodes if local != peer}
    ready: dict[str, int] = {}
    qualified: dict[str, tuple[int, int]] = {}
    soaks: dict[str, tuple[str, int, int, int, int, int, int, int, int, int, int]] = {}
    snapshots: set[tuple[str, str]] = set()
    inputs: set[tuple[str, str]] = set()
    measured: dict[tuple[str, str], tuple[str, int]] = {}
    hud: dict[tuple[str, str], tuple[str, int]] = {}
    rtt_samples: dict[tuple[str, str], int] = {}

    for node in nodes:
        path = log_dir / f"client-{node}.log"
        if not path.is_file():
            raise EvidenceError(f"missing client log for {node}")
        text = path.read_text(encoding="utf-8", errors="replace")
        if "SPURFIRE_GODOT_P2P_QUALIFY_FAILED" in text:
            raise EvidenceError(f"client {node} reported qualification failure")
        for match in READY.finditer(text):
            ready[match[1]] = int(match[2])
        for match in SNAPSHOT.finditer(text):
            sender = player_to_node.get(match[2])
            if sender is not None:
                snapshots.add((match[1], sender))
        for match in INPUT.finditer(text):
            sender = player_to_node.get(match[2])
            if sender is not None:
                inputs.add((match[1], sender))
        for match in PAIR.finditer(text):
            destination = measured if match[1] == "MEASURED" else hud
            key = (match[2], match[3])
            value = (normalize_route(match[4]), int(match[5]))
            if key in destination and destination[key] != value:
                raise EvidenceError(f"conflicting {match[1].lower()} evidence for {key}")
            destination[key] = value
        for match in RTT_READY.finditer(text):
            rtt_samples[(match[1], match[2])] = int(match[3])
        for match in QUALIFIED.finditer(text):
            qualified[match[1]] = (int(match[2]), int(match[3]))
        for match in SOAK.finditer(text):
            soaks[match[1]] = (
                match[2],
                int(match[3]),
                int(match[4]),
                int(match[5]),
                int(match[6]),
                int(match[7]),
                int(match[8]),
                int(match[9]),
                int(match[10]),
                int(match[11]),
                int(match[12]),
            )

    peer_count = len(nodes) - 1
    if ready != {node: peer_count for node in nodes}:
        raise EvidenceError(f"incomplete ready barrier: {ready}")
    if set(measured) != expected:
        raise EvidenceError(f"measured matrix is missing {sorted(expected - set(measured))}")
    if set(hud) != expected:
        raise EvidenceError(f"HUD matrix is missing {sorted(expected - set(hud))}")
    if set(rtt_samples) != expected or any(samples < 5 for samples in rtt_samples.values()):
        raise EvidenceError(f"RTT sample windows are incomplete: {rtt_samples}")
    expected_snapshots = {(node, nodes[0]) for node in nodes[1:]}
    expected_inputs = {(nodes[0], node) for node in nodes[1:]}
    if snapshots != expected_snapshots:
        raise EvidenceError(
            f"authority snapshot flow is incomplete: missing={sorted(expected_snapshots - snapshots)} "
            f"extra={sorted(snapshots - expected_snapshots)}"
        )
    if inputs != expected_inputs:
        raise EvidenceError(
            f"follower input flow is incomplete: missing={sorted(expected_inputs - inputs)} "
            f"extra={sorted(inputs - expected_inputs)}"
        )
    if set(qualified) != set(nodes) or any(
        peers != peer_count for peers, _ in qualified.values()
    ) or any(qualified[node][1] < 1 for node in nodes[1:]):
        raise EvidenceError(f"incomplete qualification barrier: {qualified}")
    for pair in sorted(expected):
        if measured[pair] != hud[pair]:
            raise EvidenceError(
                f"HUD mismatch for {pair}: measured={measured[pair]} hud={hud[pair]}"
            )

    route_counts = Counter(route for route, _ in measured.values())
    direct_rtts = [rtt for route, rtt in measured.values() if route == "DIRECT"]
    if not direct_rtts:
        raise EvidenceError("matrix did not establish a direct path")
    direct_median = int(statistics.median(direct_rtts))
    if direct_median >= 80:
        raise EvidenceError(f"direct median RTT {direct_median}ms is not below 80ms")
    classes = ",".join(f"{name}:{route_counts[name]}" for name in sorted(route_counts))
    marker = (
        f"SPURFIRE_GODOT_P2P_MATRIX_OK peers={len(nodes)} "
        f"directed_routes={len(expected)} hud_matches={len(expected)} "
        f"authority_snapshot_receivers={len(expected_snapshots)} "
        f"authority_input_senders={len(expected_inputs)} direct_median_rtt_ms={direct_median} "
        f"route_classes={classes}"
    )
    if minimum_soak_ms > 0:
        if set(soaks) != set(nodes):
            raise EvidenceError(f"incomplete soak evidence: {soaks}")
        authority = soaks[nodes[0]]
        if authority[0] != "authority" or authority[1] < minimum_soak_ms:
            raise EvidenceError(f"invalid authority soak evidence: {authority}")
        minimum_inputs_per_sender = minimum_soak_ms * 10 // 1000
        if authority[4] < minimum_inputs_per_sender:
            raise EvidenceError(
                f"authority per-sender input starvation: observed={authority[4]} "
                f"minimum={minimum_inputs_per_sender}"
            )
        if authority[3] < minimum_inputs_per_sender * peer_count:
            raise EvidenceError("authority aggregate input evidence is inconsistent")
        follower_rows = [soaks[node] for node in nodes[1:]]
        minimum_snapshots = minimum_soak_ms * 16 // 1000
        minimum_presentation_samples = minimum_soak_ms * 30 // 1000
        for node, row in zip(nodes[1:], follower_rows, strict=True):
            if row[0] != "follower" or row[1] < minimum_soak_ms:
                raise EvidenceError(f"invalid follower soak evidence for {node}: {row}")
            if row[2] < minimum_snapshots:
                raise EvidenceError(
                    f"follower {node} snapshot starvation: observed={row[2]} "
                    f"minimum={minimum_snapshots}"
                )
            if row[5] > 200:
                raise EvidenceError(f"follower {node} snapshot gap {row[5]}ms exceeds 200ms")
            if row[6] < 30000:
                raise EvidenceError(f"follower {node} motion span {row[6]}mm is below 30000mm")
            if row[7] > 200:
                raise EvidenceError(f"follower {node} final snapshot age {row[7]}ms exceeds 200ms")
            if row[8] < minimum_presentation_samples:
                raise EvidenceError(
                    f"follower {node} presentation samples {row[8]} are below "
                    f"{minimum_presentation_samples}"
                )
            if row[9] > 200:
                raise EvidenceError(
                    f"follower {node} presentation desync {row[9]}ms exceeds 200ms"
                )
        marker += (
            "\nSPURFIRE_GODOT_P2P_SOAK_OK"
            f" peers={len(nodes)} duration_ms={min(row[1] for row in soaks.values())}"
            f" min_sender_inputs={authority[4]}"
            f" peak_gap_ms={max(row[5] for row in follower_rows)}"
            f" max_last_age_ms={max(row[7] for row in follower_rows)}"
            f" min_motion_span_mm={min(row[6] for row in follower_rows)}"
            f" min_presentation_samples={min(row[8] for row in follower_rows)}"
            f" peak_presentation_desync_ms={max(row[9] for row in follower_rows)}"
        )
    return marker


def main() -> int:
    if len(sys.argv) not in (3, 4):
        print(f"usage: {sys.argv[0]} LOG_DIR NODE_CSV [MINIMUM_SOAK_MS]", file=sys.stderr)
        return 2
    try:
        minimum_soak_ms = int(sys.argv[3]) if len(sys.argv) == 4 else 0
        if minimum_soak_ms < 0:
            raise ValueError
        print(
            validate(
                Path(sys.argv[1]),
                [node for node in sys.argv[2].split(",") if node],
                minimum_soak_ms,
            )
        )
    except ValueError:
        print("ERROR: MINIMUM_SOAK_MS must be a non-negative integer", file=sys.stderr)
        return 2
    except EvidenceError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
