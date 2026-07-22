from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-godot-p2p-evidence.py"
SPEC = importlib.util.spec_from_file_location("godot_p2p_evidence", SCRIPT)
assert SPEC and SPEC.loader
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)


class GodotP2PEvidenceTests(unittest.TestCase):
    def write_matrix(
        self,
        root: Path,
        *,
        mismatch: bool = False,
        soak: bool = False,
        peak_gap_ms: int = 80,
        presentation_desync_ms: int = 40,
    ) -> None:
        players = {
            "a": "00000000-0000-4000-8000-000000000002",
            "b": "00000000-0000-4000-8000-000000000003",
        }
        for local, peer, rtt in [("a", "b", 10), ("b", "a", 12)]:
            hud_rtt = rtt + 1 if mismatch and local == "a" else rtt
            lines = [
                f"SPURFIRE_GODOT_P2P_READY local={local} peers=1",
                (
                    f"SPURFIRE_GODOT_P2P_INPUT local=a sender={players['b']}"
                    if local == "a"
                    else f"SPURFIRE_GODOT_P2P_SNAPSHOT local=b sender={players['a']}"
                ),
                f"SPURFIRE_GODOT_P2P_MEASURED local={local} peer={peer} route=Direct rtt_ms={rtt}",
                f"SPURFIRE_GODOT_P2P_HUD local={local} peer={peer} route=DIRECT rtt_ms={hud_rtt}",
                f"SPURFIRE_GODOT_P2P_RTT_READY local={local} peer={peer} samples=9",
                f"SPURFIRE_GODOT_P2P_QUALIFIED local={local} peers=1 snapshots=4",
            ]
            if soak:
                role = "authority" if local == "a" else "follower"
                snapshots = 0 if local == "a" else 500
                inputs = 400 if local == "a" else 0
                motion = 0 if local == "a" else 40000
                presentation_samples = 0 if local == "a" else 1800
                lines.append(
                    f"SPURFIRE_GODOT_P2P_SOAK local={local} role={role} duration_ms=30000 "
                    f"snapshots={snapshots} inputs={inputs} min_sender_inputs={inputs} "
                    f"peak_gap_ms={peak_gap_ms} "
                    f"motion_span_mm={motion} last_age_ms=40 "
                    f"presentation_samples={presentation_samples} "
                    f"presentation_desync_ms={presentation_desync_ms} rejects=0"
                )
            (root / f"client-{local}.log").write_text(
                "\n".join(lines) + "\n",
                encoding="utf-8",
            )

    def test_exact_matrix_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_matrix(root)
            marker = EVIDENCE.validate(root, ["a", "b"])
        self.assertIn("peers=2 directed_routes=2 hud_matches=2", marker)
        self.assertIn("authority_snapshot_receivers=1 authority_input_senders=1", marker)
        self.assertIn("direct_median_rtt_ms=11", marker)

    def test_hud_mismatch_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_matrix(root, mismatch=True)
            with self.assertRaises(EVIDENCE.EvidenceError):
                EVIDENCE.validate(root, ["a", "b"])

    def test_missing_client_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(EVIDENCE.EvidenceError):
                EVIDENCE.validate(Path(directory), ["a", "b"])

    def test_soak_evidence_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_matrix(root, soak=True)
            marker = EVIDENCE.validate(root, ["a", "b"], 30000)
        self.assertIn("SPURFIRE_GODOT_P2P_SOAK_OK peers=2 duration_ms=30000", marker)
        self.assertIn("peak_gap_ms=80", marker)

    def test_soak_gap_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_matrix(root, soak=True, peak_gap_ms=201)
            with self.assertRaises(EVIDENCE.EvidenceError):
                EVIDENCE.validate(root, ["a", "b"], 30000)

    def test_presentation_desync_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_matrix(root, soak=True, presentation_desync_ms=201)
            with self.assertRaises(EVIDENCE.EvidenceError):
                EVIDENCE.validate(root, ["a", "b"], 30000)


if __name__ == "__main__":
    unittest.main()
