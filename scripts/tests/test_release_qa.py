from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load_script(name: str):
    path = ROOT / "scripts" / name
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


AGGREGATE = load_script("aggregate-playtest.py")
LIFECYCLE = load_script("check-alpha-lifecycle-evidence.py")
EVIDENCE = load_script("check-alpha-evidence.py")
SHA = "0123456789abcdef0123456789abcdef01234567"


def base_record(event: str, session: str = "session-local-1") -> dict:
    return {"schema_version": 1, "build_commit": SHA, "session_id": session, "event_type": event}


class AggregateTests(unittest.TestCase):
    def records(self) -> list[dict]:
        rows = [
            {**base_record("session_started"), "timestamp_ms": 0},
            {**base_record("dive_started"), "actor": "rider-a", "dive_id": 1},
            {
                **base_record("dive_finalized"),
                "actor": "rider-a",
                "dive_id": 1,
                "shots_fired": 4,
                "shots_hit": 2,
                "death_within_3s": True,
                "direction_was_clamped": True,
                "time_to_remount_ticks": 480,
            },
            {**base_record("combat_shot"), "actor": "rider-a", "context": "gallop", "accepted": True, "hit": True},
            {**base_record("combat_shot"), "actor": "rider-a", "context": "gallop", "accepted": True, "hit": False},
            {**base_record("notification"), "actor": "rider-a", "text": "FLYING DISMOUNT"},
            {**base_record("reload_rejected"), "actor": "rider-a", "reason": "recovering"},
            {
                **base_record("render_sample"),
                "actor": "rider-a",
                "refresh_hz": 144,
                "frame_delta_ms": 6.94,
                "linear_jerk": 2.0,
                "angular_jerk": 3.0,
                "repeated_transform": False,
            },
            {**base_record("session_ended"), "timestamp_ms": 900_000},
        ]
        for index, row in enumerate(rows, 1):
            row["_source"] = f"fixture:{index}"
        return rows

    def test_deterministic_metrics(self):
        first = AGGREGATE.aggregate(self.records(), strict=True)
        second = AGGREGATE.aggregate(self.records(), strict=True)
        self.assertEqual(first, second)
        metrics = first["dive_metrics"]
        self.assertEqual(metrics["dives_per_player_15_minutes"], 1.0)
        self.assertEqual(metrics["airborne_hit_rate"], 0.5)
        self.assertEqual(metrics["gallop_hit_rate"], 0.5)
        self.assertEqual(metrics["airborne_hit_rate_uplift_percent"], 0.0)
        self.assertEqual(metrics["landing_death_rate"], 1.0)
        self.assertEqual(metrics["remount_seconds_median"], 8.0)
        self.assertEqual(first["reload_rejection_counts"], {"recovering": 1})

    def test_secret_field_rejected(self):
        with self.assertRaises(AGGREGATE.InputError):
            AGGREGATE._scan_secret_free({"endpoint": "redacted"})
        with self.assertRaises(AGGREGATE.InputError):
            AGGREGATE._scan_secret_free({"message": "Bearer not-allowed"})


class LifecycleTests(unittest.TestCase):
    def simulated(self) -> dict:
        roster = {
            "roster_hash": "a" * 64,
            "network_generation": 1,
            "session_generation": 1,
            "roster_revision": 2,
            "actors": ["rider-a", "rider-b"],
        }
        events = []
        for client in ("a", "b"):
            events.extend(
                [
                    {"event": "download_verified", "client": client},
                    {"event": "joined", "client": client},
                    {"event": "roster_observed", "client": client, **roster},
                    {"event": "network_health", "client": client, "route_class": "unknown", "rtt_ms": None},
                    {"event": "m2_coherent", "client": client},
                    {"event": "leave_confirmed", "client": client},
                ]
            )
        events.extend(
            [
                {"event": "control_service_membership", "member": False},
                {"event": "simulated_cleanup_complete"},
            ]
        )
        return {
            "schema_version": 1,
            "source_sha": SHA,
            "mode": "simulated",
            "candidate_archives": {"a": "1" * 64, "b": "2" * 64},
            "events": events,
        }

    def test_simulated_is_valid_but_not_release_qualifying(self):
        result = LIFECYCLE.validate(self.simulated(), require_live=False)
        self.assertFalse(result["release_qualifying"])
        with self.assertRaises(LIFECYCLE.EvidenceError):
            LIFECYCLE.validate(self.simulated(), require_live=True)

    def test_private_live_requires_ordered_cleanup(self):
        document = self.simulated()
        document["mode"] = "private_live"
        document["events"] = [event for event in document["events"] if event["event"] != "simulated_cleanup_complete"]
        document["events"].extend(
            [
                {"event": "exact_absence_observed", "observation": 1, "stable_id_digest": "sha256:" + "3" * 64, "completed_ms": 1000, "exact_id_present": False},
                {"event": "exact_absence_observed", "observation": 2, "stable_id_digest": "sha256:" + "3" * 64, "completed_ms": 6000, "exact_id_present": False},
                {"event": "vault_erasure_verified", "completed_ms": 7000},
                {"event": "dedicated_absent", "completed_ms": 8000, "lease_released": True},
            ]
        )
        result = LIFECYCLE.validate(document, require_live=True)
        self.assertTrue(result["release_qualifying"])
        document["events"][-3]["completed_ms"] = 5999
        with self.assertRaises(LIFECYCLE.EvidenceError):
            LIFECYCLE.validate(document, require_live=True)


class EvidenceTests(unittest.TestCase):
    def manifest(self) -> dict:
        gates = {name: True for name in EVIDENCE.REQUIRED_GATES}
        artifacts = [
            {
                "platform": platform,
                "sha256": str(index) * 64,
                "provenance_verified": True,
                "sbom_present": True,
                "launch_smoke_passed": True,
            }
            for index, platform in enumerate(EVIDENCE.REQUIRED_PLATFORMS, 1)
        ]
        runs = {
            name: {"id": index, "head_sha": SHA, "conclusion": "success"}
            for index, name in enumerate(("ci", "client_preflight", "private_live_lifecycle"), 1)
        }
        return {
            "schema_version": 1,
            "version": "0.2.0",
            "source_sha": SHA,
            "blockers": [],
            "gates": gates,
            "runs": runs,
            "artifacts": artifacts,
            "distribution_trust": {
                "macos": {"developer_id_signed": True, "notarized": True},
                "windows": {"authenticode_signed": True},
            },
            "approvals": {
                "activation": {"approved": True, "evidence_digest": "sha256:" + "4" * 64},
                "release": {"approved": True, "evidence_digest": "sha256:" + "5" * 64},
            },
        }

    def test_complete_manifest(self):
        result = EVIDENCE.validate(self.manifest(), version="0.2.0", source_sha=SHA)
        self.assertTrue(result["ok"])

    def test_unsigned_platforms_block_release(self):
        manifest = self.manifest()
        manifest["distribution_trust"]["macos"]["notarized"] = False
        with self.assertRaises(EVIDENCE.ManifestError):
            EVIDENCE.validate(manifest, version="0.2.0", source_sha=SHA)


class CommandTests(unittest.TestCase):
    def test_release_tag_binding_allows_only_metadata_child_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            subprocess.run(["git", "init", "-q", repo], check=True)
            subprocess.run(["git", "-C", repo, "config", "user.email", "qa@example.invalid"], check=True)
            subprocess.run(["git", "-C", repo, "config", "user.name", "Release QA"], check=True)
            (repo / "source.txt").write_text("qualified\n", encoding="utf-8")
            subprocess.run(["git", "-C", repo, "add", "source.txt"], check=True)
            subprocess.run(["git", "-C", repo, "commit", "-qm", "qualified source"], check=True)
            source_sha = subprocess.check_output(
                ["git", "-C", repo, "rev-parse", "HEAD"], text=True
            ).strip()
            evidence_dir = repo / "docs" / "release-evidence"
            evidence_dir.mkdir(parents=True)
            (evidence_dir / "0.2.0.json").write_text(
                json.dumps({"source_sha": source_sha}), encoding="utf-8"
            )
            (repo / "docs" / "release-notes-0.2.0.md").write_text(
                "# Evidence\n", encoding="utf-8"
            )
            subprocess.run(["git", "-C", repo, "add", "docs"], check=True)
            subprocess.run(["git", "-C", repo, "commit", "-qm", "release evidence"], check=True)
            subprocess.run(
                [ROOT / "scripts/check-release-tag-binding.sh", "0.2.0", "HEAD"],
                cwd=repo,
                check=True,
                capture_output=True,
                text=True,
            )
            (repo / "source.txt").write_text("changed after qualification\n", encoding="utf-8")
            subprocess.run(["git", "-C", repo, "commit", "-qam", "forbidden code change"], check=True)
            failed = subprocess.run(
                [ROOT / "scripts/check-release-tag-binding.sh", "0.2.0", "HEAD"],
                cwd=repo,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(failed.returncode, 0)

    def test_smoke_marker_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "godot.log"
            log.write_text(
                "\n".join(
                    [
                        "SPURFIRE_GODOT_SMOKE_OK",
                        "SPURFIRE_POLISH_SMOKE_OK",
                        "SPURFIRE_COMBAT_UI_SMOKE_OK",
                        "SPURFIRE_ALPHA_LOBBY_SMOKE_OK",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            subprocess.run([ROOT / "scripts/check-alpha-smoke-log.sh", log], check=True)
            log.write_text("SPURFIRE_GODOT_SMOKE_OK\n", encoding="utf-8")
            failed = subprocess.run([ROOT / "scripts/check-alpha-smoke-log.sh", log], check=False)
            self.assertNotEqual(failed.returncode, 0)

    def test_workflows_fail_closed_and_do_not_overwrite(self):
        preflight = (ROOT / ".github/workflows/client-release.yml").read_text(encoding="utf-8")
        publisher = (ROOT / ".github/workflows/client-publish.yml").read_text(encoding="utf-8")
        packages = (ROOT / ".github/workflows/packages.yml").read_text(encoding="utf-8")
        self.assertNotIn("git tag", preflight)
        self.assertNotIn("git push", preflight)
        self.assertNotIn("gh release create", preflight)
        self.assertIn("environment: alpha-release", publisher)
        self.assertIn("refusing to overwrite it", publisher)
        self.assertIn("--expected-sha256", publisher)
        self.assertNotIn(".head_branch == $tag", publisher)
        self.assertNotIn('--source-sha "$GITHUB_SHA"', preflight)
        self.assertNotIn('--source-sha "$GITHUB_SHA"', packages)
        self.assertIn("check-release-tag-binding.sh", preflight)
        self.assertIn("check-release-tag-binding.sh", publisher)
        self.assertIn("github.ref == 'refs/heads/main'", packages)

    def test_candidate_metadata_is_nonpublishing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            output = root / "output"
            source.mkdir()
            for name in {
                "Spurfire-linux-x86_64.tar.gz",
                "Spurfire-macos-universal.zip",
                "Spurfire-windows-x86_64.zip",
            }:
                (source / name).write_bytes(name.encode())
            subprocess.run(
                [
                    "python3",
                    ROOT / "scripts/make-alpha-candidate-metadata.py",
                    "--input-dir",
                    source,
                    "--output-dir",
                    output,
                    "--source-sha",
                    SHA,
                    "--run-id",
                    "1",
                    "--run-attempt",
                    "1",
                    "--event",
                    "workflow_dispatch",
                    "--provenance-verified",
                ],
                check=True,
            )
            manifest = json.loads((output / "candidate-manifest.json").read_text())
            self.assertFalse(manifest["release_eligible"])
            self.assertEqual(len(manifest["artifacts"]), 3)
            self.assertTrue((output / "SHA256SUMS").is_file())
            self.assertTrue((output / "candidate.spdx.json").is_file())

    def test_two_client_entry_point_with_simulated_driver(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            client_a = root / "a.zip"
            client_b = root / "b.zip"
            client_a.write_bytes(b"client-a")
            client_b.write_bytes(b"client-b")
            driver = root / "driver.py"
            driver.write_text(
                """#!/usr/bin/env python3
import hashlib, json, os
roster = {'roster_hash': 'a' * 64, 'network_generation': 1, 'session_generation': 1, 'roster_revision': 2, 'actors': ['rider-a', 'rider-b']}
events = []
for client in ('a', 'b'):
    events += [
        {'event': 'download_verified', 'client': client},
        {'event': 'joined', 'client': client},
        {'event': 'roster_observed', 'client': client, **roster},
        {'event': 'network_health', 'client': client, 'route_class': 'unknown', 'rtt_ms': None},
        {'event': 'm2_coherent', 'client': client},
        {'event': 'leave_confirmed', 'client': client},
    ]
events += [{'event': 'control_service_membership', 'member': False}, {'event': 'simulated_cleanup_complete'}]
def sha(path): return hashlib.sha256(open(path, 'rb').read()).hexdigest()
doc = {'schema_version': 1, 'source_sha': os.environ['SOURCE_SHA'], 'mode': 'simulated', 'candidate_archives': {'a': sha(os.environ['CLIENT_A']), 'b': sha(os.environ['CLIENT_B'])}, 'events': events}
open(os.environ['EVIDENCE_OUTPUT'], 'w').write(json.dumps(doc))
""",
                encoding="utf-8",
            )
            driver.chmod(0o755)
            evidence = root / "evidence.json"
            env = os.environ.copy()
            for name in ("TS_CLIENT_ID", "TS_CLIENT_SECRET", "TS_AUTHKEY", "TS_API_TOKEN", "SPURFIRE_CAPABILITY", "SPURFIRE_JOIN_CODE"):
                env.pop(name, None)
            env["SPURFIRE_ALPHA_SOURCE_SHA"] = SHA
            subprocess.run(
                [
                    ROOT / "scripts/run-alpha-two-client.sh",
                    "--client-a",
                    client_a,
                    "--client-b",
                    client_b,
                    "--driver",
                    driver,
                    "--output",
                    evidence,
                ],
                check=True,
                env=env,
            )
            self.assertTrue(evidence.is_file())


if __name__ == "__main__":
    unittest.main()
