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
QUALIFIED = re.compile(r"SPURFIRE_GODOT_P2P_QUALIFIED local=(\w+) peers=(\d+) snapshots=(\d+)")


def normalize_route(route: str) -> str:
    upper = route.upper().replace("_", " ")
    if "PEER" in upper and "RELAY" in upper:
        return "PEER RELAY"
    if "DERP" in upper:
        return "DERP"
    if "DIRECT" in upper:
        return "DIRECT"
    raise EvidenceError(f"unknown route class {route!r}")


def validate(log_dir: Path, nodes: list[str]) -> str:
    if len(nodes) < 2 or len(nodes) != len(set(nodes)):
        raise EvidenceError("node roster must contain distinct peers")
    player_to_node = {
        f"00000000-0000-4000-8000-{index + 2:012d}": node
        for index, node in enumerate("abcdefgh")
    }
    expected = {(local, peer) for local in nodes for peer in nodes if local != peer}
    ready: dict[str, int] = {}
    qualified: dict[str, tuple[int, int]] = {}
    snapshots: set[tuple[str, str]] = set()
    measured: dict[tuple[str, str], tuple[str, int]] = {}
    hud: dict[tuple[str, str], tuple[str, int]] = {}

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
        for match in PAIR.finditer(text):
            destination = measured if match[1] == "MEASURED" else hud
            key = (match[2], match[3])
            value = (normalize_route(match[4]), int(match[5]))
            if key in destination and destination[key] != value:
                raise EvidenceError(f"conflicting {match[1].lower()} evidence for {key}")
            destination[key] = value
        for match in QUALIFIED.finditer(text):
            qualified[match[1]] = (int(match[2]), int(match[3]))

    peer_count = len(nodes) - 1
    if ready != {node: peer_count for node in nodes}:
        raise EvidenceError(f"incomplete ready barrier: {ready}")
    if set(measured) != expected:
        raise EvidenceError(f"measured matrix is missing {sorted(expected - set(measured))}")
    if set(hud) != expected:
        raise EvidenceError(f"HUD matrix is missing {sorted(expected - set(hud))}")
    if snapshots != expected:
        raise EvidenceError(f"snapshot matrix is missing {sorted(expected - snapshots)}")
    if set(qualified) != set(nodes) or any(
        peers != peer_count or count < peer_count for peers, count in qualified.values()
    ):
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
    return (
        f"SPURFIRE_GODOT_P2P_MATRIX_OK peers={len(nodes)} "
        f"directed_routes={len(expected)} hud_matches={len(expected)} "
        f"snapshot_directions={len(expected)} direct_median_rtt_ms={direct_median} "
        f"route_classes={classes}"
    )


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} LOG_DIR NODE_CSV", file=sys.stderr)
        return 2
    try:
        print(validate(Path(sys.argv[1]), [node for node in sys.argv[2].split(",") if node]))
    except EvidenceError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
