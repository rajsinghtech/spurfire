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
CANDIDATE = load_script("make-alpha-candidate-metadata.py")
REHEARSAL_CANDIDATE = load_script("make-rehearsal-candidate-metadata.py")
SHA = "0123456789abcdef0123456789abcdef01234567"


def write_candidate_inputs(root: Path, *, trusted: bool = False) -> Path:
    source = root / "source"
    source.mkdir()
    archives = {
        "Spurfire-linux-arm64.tar.gz": "linux-arm64",
        "Spurfire-linux-x86_64.tar.gz": "linux-x86_64",
        "Spurfire-macos-universal.zip": "macos-universal",
        "Spurfire-windows-x86_64.zip": "windows-x86_64",
    }
    for name in archives:
        (source / name).write_bytes(name.encode())

    def archive_sha(name: str) -> str:
        return hashlib.sha256((source / name).read_bytes()).hexdigest()

    records = {
        "linux-arm64-trust.json": {
            "schema_version": 1,
            "platform": "linux-arm64",
            "source_sha": SHA,
            "archive": "Spurfire-linux-arm64.tar.gz",
            "archive_sha256": archive_sha("Spurfire-linux-arm64.tar.gz"),
            "launch_smoke_passed": True,
            "signature": "unsigned_archive",
        },
        "linux-trust.json": {
            "schema_version": 1,
            "platform": "linux-x86_64",
            "source_sha": SHA,
            "archive": "Spurfire-linux-x86_64.tar.gz",
            "archive_sha256": archive_sha("Spurfire-linux-x86_64.tar.gz"),
            "launch_smoke_passed": True,
            "signature": "unsigned_archive",
        },
        "macos-trust.json": {
            "schema_version": 1,
            "platform": "macos-universal",
            "source_sha": SHA,
            "archive": "Spurfire-macos-universal.zip",
            "archive_sha256": archive_sha("Spurfire-macos-universal.zip"),
            "launch_smoke_passed": True,
            "signature": "developer_id" if trusted else "ad_hoc",
            "developer_id_signed": trusted,
            "notarized": trusted,
            "verification": {
                "codesign_deep_strict": trusted,
                "notarization_stapled": trusted,
                "gatekeeper_assessment": trusted,
                "team_id": "A" * 10 if trusted else "",
            },
        },
        "windows-trust.json": {
            "schema_version": 1,
            "platform": "windows-x86_64",
            "source_sha": SHA,
            "archive": "Spurfire-windows-x86_64.zip",
            "archive_sha256": archive_sha("Spurfire-windows-x86_64.zip"),
            "launch_smoke_passed": True,
            "signature": "authenticode" if trusted else "unsigned",
            "authenticode_signed": trusted,
            "verification": {
                "status": "Valid" if trusted else "NotSigned",
                "timestamp_verified": trusted,
                "signer_certificate_sha256": "a" * 64 if trusted else "",
            },
        },
    }
    for name, record in records.items():
        (source / name).write_text(json.dumps(record), encoding="utf-8")
    return source


def run_candidate_metadata(source: Path, output: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
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
            *extra,
        ],
        check=False,
        capture_output=True,
        text=True,
    )


def base_record(event: str, session: str = "session-local-1") -> dict:
    return {"schema_version": 1, "build_commit": SHA, "session_id": session, "event_type": event}


class RehearsalCandidateTests(unittest.TestCase):
    def fixture(self, root: Path) -> tuple[Path, Path]:
        source = root / "source"
        output = root / "output"
        source.mkdir()
        archive = source / REHEARSAL_CANDIDATE.ARCHIVE
        archive.write_bytes(b"fixed rehearsal archive")
        record = {
            "schema_version": 2,
            "candidate_mode": "rehearsal",
            "platform": "macos-universal",
            "control_origin": REHEARSAL_CANDIDATE.APPROVED_REHEARSAL_ORIGIN,
            "source_sha": SHA,
            "archive": archive.name,
            "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
            "launch_smoke_passed": True,
            "signature": "ad_hoc",
            "signing_trust": "untrusted",
            "release_eligible": False,
            "publication": "forbidden",
        }
        (source / "macos-rehearsal-trust.json").write_text(json.dumps(record), encoding="utf-8")
        return source, output

    def execute(self, source: Path, output: Path, origin: str | None = None) -> int:
        return REHEARSAL_CANDIDATE.main([
            "--input-dir", str(source),
            "--output-dir", str(output),
            "--source-sha", SHA,
            "--control-origin", origin or REHEARSAL_CANDIDATE.APPROVED_REHEARSAL_ORIGIN,
        ])

    def test_exact_record_is_permanently_nonpublishing(self):
        with tempfile.TemporaryDirectory() as temporary:
            source, output = self.fixture(Path(temporary))
            self.assertEqual(self.execute(source, output), 0)
            manifest = json.loads((output / "rehearsal-candidate-manifest.json").read_text())
            self.assertTrue(manifest["candidate_only"])
            self.assertFalse(manifest["release_eligible"])
            self.assertEqual(manifest["publication"], "forbidden")

    def test_swapped_bindings_and_publish_attempts_are_rejected(self):
        mutations = {
            "control_origin": "https://evil.example",
            "source_sha": "f" * 40,
            "archive_sha256": "0" * 64,
            "signature": "developer_id",
            "signing_trust": "trusted",
            "release_eligible": True,
            "publication": "protected_manual_only",
        }
        for field, value in mutations.items():
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                source, output = self.fixture(Path(temporary))
                path = source / "macos-rehearsal-trust.json"
                record = json.loads(path.read_text())
                record[field] = value
                path.write_text(json.dumps(record), encoding="utf-8")
                self.assertEqual(self.execute(source, output), 1)


class ReleaseMetadataTests(unittest.TestCase):
    def test_current_release_metadata_contract(self):
        result = subprocess.run(
            [ROOT / "scripts/check-release-metadata.sh"],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout.strip(), r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")


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
            {
                **base_record("m3_interval"),
                "actor_slot": 0,
                "tick_start": 0,
                "tick_end": 599,
                "mounted_ticks": 420,
                "on_foot_ticks": 180,
                "roll_ticks": 30,
                "spook_stun_ticks": 36,
                "horse_losses": 1,
                "remounts": 1,
                "running_mount_attempts": 2,
                "running_remounts": 1,
                "duel_wins": 2,
                "on_foot_vs_mounted_duels": 4,
                "on_foot_vs_mounted_wins": 1,
                "post_spook_deaths": 1,
                "charge_ticks": 120,
                "full_spur_ticks": 60,
                "charge_starts": 1,
                "instant_returns": 1,
                "charged_duels": 4,
                "charged_duel_wins": 3,
                "uncharged_duels": 4,
                "uncharged_duel_wins": 2,
                "spur_points_jump": 4,
                "spur_points_clean_landing": 2,
                "spur_points_near_miss": 3,
                "spur_points_mounted_hit": 2,
                "spur_points_mounted_elimination": 6,
                "spur_points_saddle_dive_elimination": 8,
            },
            {
                **base_record("m4_spend"),
                "actor_slot": 0,
                "tick": 300,
                "kind": "majestic_charge",
            },
            {
                **base_record("m3_remount"),
                "actor_slot": 0,
                "lose_horse_to_remount_ticks": 1800,
                "running_mount": True,
            },
            {**base_record("m3_horse_lost"), "actor_slot": 0, "notification_points": 15},
            {
                **base_record("m5_interval"),
                "actor_slot": 0,
                "tick_start": 0,
                "tick_end": 1199,
                "alive_ticks": 1000,
                "dead_ticks": 200,
                "reveal_ticks": 100,
                "reveal_score_gain": 10,
                "normal_score_gain": 90,
                "encounter_ticks": 300,
                "objective_proximity_ticks": 180,
                "current_gap_ticks": 6000,
                "max_gap_ticks": 6000,
            },
            {
                **base_record("m5_match_result"),
                "tick": 54000,
                "match_duration_ticks": 54000,
                "players": [
                    {
                        "actor_slot": 0,
                        "score": 500,
                        "eliminations": 3,
                        "assists": 1,
                        "deaths": 2,
                        "score_breakdown": {
                            "elimination": 300,
                            "assist": 50,
                            "horse_bolt": 0,
                            "saddle_dive_bonus": 0,
                            "mounted_long_hit": 0,
                            "objective": 150,
                            "most_wanted_elimination": 0,
                            "most_wanted_survival": 0,
                        },
                        "winner": True,
                    },
                    {
                        "actor_slot": 1,
                        "score": 250,
                        "eliminations": 1,
                        "assists": 0,
                        "deaths": 3,
                        "score_breakdown": {
                            "elimination": 100,
                            "assist": 0,
                            "horse_bolt": 0,
                            "saddle_dive_bonus": 0,
                            "mounted_long_hit": 0,
                            "objective": 150,
                            "most_wanted_elimination": 0,
                            "most_wanted_survival": 0,
                        },
                        "winner": False,
                    },
                ],
            },
            {**base_record("m5_survey"), "would_play_again": True},
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
        m3 = first["m3_metrics"]
        self.assertEqual(m3["mounted_time_share"], 0.7)
        self.assertEqual(m3["lose_horse_to_remount_seconds_median"], 30.0)
        self.assertEqual(m3["running_mount_success_rate"], 0.5)
        self.assertEqual(m3["on_foot_vs_mounted_win_rate"], 0.25)
        self.assertEqual(m3["post_spook_death_rate"], 1.0)
        self.assertEqual(m3["bolt_notification_coverage"], 1.0)
        m4 = first["m4_metrics"]
        self.assertEqual(m4["spur_points_total"], 25)
        self.assertEqual(m4["movement_style_point_share"], 0.24)
        self.assertEqual(m4["charge_time_share"], 0.2)
        self.assertEqual(m4["full_meter_hoard_time_share"], 0.1)
        self.assertEqual(m4["instant_return_spend_share"], 0.5)
        self.assertEqual(m4["charge_win_rate_delta"], 0.25)
        self.assertEqual(m4["charge_starts_per_actor_median"], 1)
        self.assertEqual(m4["charge_starts_per_actor_p75"], 1)
        self.assertEqual(m4["first_charge_minutes_median"], 0.083333)
        m5 = first["m5_metrics"]
        self.assertEqual(m5["completed_matches"], 1)
        self.assertEqual(m5["winner_score_median"], 500)
        self.assertEqual(m5["winner_score_target_share"], 1.0)
        self.assertEqual(m5["winner_non_elimination_categories_median"], 2)
        self.assertEqual(m5["objective_score_share"], 0.4)
        self.assertEqual(m5["max_convergence_gap_seconds"], 100.0)
        self.assertEqual(m5["intervals_over_90s_convergence_gap"], 1)
        self.assertEqual(m5["would_play_again_rate"], 1.0)

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
        runs["private_live_lifecycle"].update(
            {
                "repository": "trusted/live-evidence",
                "workflow_path": ".github/workflows/private-live.yml",
                "evidence_artifact": "private-live-evidence",
                "evidence_file": "private-live.json",
                "evidence_sha256": "3" * 64,
            }
        )
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
                "activation": {
                    "approved": True,
                    "evidence_digest": "sha256:" + "4" * 64,
                    "repository": "rajsinghtech/spurfire",
                    "pull_request": 9,
                    "review_id": 41,
                    "reviewer": "activation-reviewer",
                },
                "release": {
                    "approved": True,
                    "evidence_digest": "sha256:" + "5" * 64,
                    "repository": "rajsinghtech/spurfire",
                    "pull_request": 9,
                    "review_id": 42,
                    "reviewer": "release-reviewer",
                },
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

    def test_self_asserted_external_bindings_block_release(self):
        manifest = self.manifest()
        manifest["runs"]["private_live_lifecycle"].pop("workflow_path")
        with self.assertRaises(EVIDENCE.ManifestError):
            EVIDENCE.validate(manifest, version="0.2.0", source_sha=SHA)

        manifest = self.manifest()
        manifest["approvals"]["release"]["reviewer"] = manifest["approvals"]["activation"]["reviewer"]
        with self.assertRaises(EVIDENCE.ManifestError):
            EVIDENCE.validate(manifest, version="0.2.0", source_sha=SHA)

    def test_publisher_resolves_external_evidence(self):
        publisher = (ROOT / ".github" / "workflows" / "client-publish.yml").read_text(
            encoding="utf-8"
        )
        for contract in (
            "ALPHA_PRIVATE_LIVE_REPOSITORY",
            "ALPHA_PRIVATE_LIVE_WORKFLOW",
            "gh run download",
            "check-alpha-lifecycle-evidence.py --require-live",
            "/reviews/$approval_review",
            "SPURFIRE_ALPHA_${approval_name^^}_APPROVED",
        ):
            self.assertIn(contract, publisher)

    def test_missing_linux_arm64_artifact_blocks_release(self):
        manifest = self.manifest()
        manifest["artifacts"] = [
            item
            for item in manifest["artifacts"]
            if item["platform"] != "linux-arm64"
        ]
        with self.assertRaises(EVIDENCE.ManifestError):
            EVIDENCE.validate(manifest, version="0.2.0", source_sha=SHA)


class PlatformCoverageTests(unittest.TestCase):
    """Every GDExtension library registration must have release coverage."""

    REGISTRATION_MAP = {
        ("linux", "x86_64"): "linux-x86_64",
        ("linux", "arm64"): "linux-arm64",
        ("windows", "x86_64"): "windows-x86_64",
        ("macos", "x86_64"): "macos-universal",
        ("macos", "arm64"): "macos-universal",
    }

    def registered_platforms(self) -> set[str]:
        text = (ROOT / "game" / "bin" / "spurfire.gdextension").read_text(encoding="utf-8")
        platforms: set[str] = set()
        in_libraries = False
        for raw_line in text.splitlines():
            line = raw_line.strip()
            if line.startswith("["):
                in_libraries = line == "[libraries]"
                continue
            if not in_libraries or "=" not in line:
                continue
            key = line.split("=", 1)[0].strip()
            parts = key.split(".")
            if len(parts) != 3:
                continue
            system, _, arch = parts
            platform = self.REGISTRATION_MAP.get((system, arch))
            self.assertIsNotNone(platform, f"unmapped GDExtension registration {key}")
            platforms.add(platform)
        return platforms

    def test_every_registered_library_has_release_coverage(self):
        preflight = (ROOT / ".github" / "workflows" / "client-release.yml").read_text(
            encoding="utf-8"
        )
        registered = self.registered_platforms()
        self.assertTrue(registered)
        self.assertEqual(registered, set(CANDIDATE.EXPECTED.values()))
        self.assertEqual(registered, set(EVIDENCE.REQUIRED_PLATFORMS))
        for platform in sorted(registered):
            with self.subTest(platform=platform):
                archive = next(
                    name
                    for name, value in CANDIDATE.EXPECTED.items()
                    if value == platform
                )
                self.assertIn(archive, preflight)
                self.assertIn(CANDIDATE.TRUST_FILES[platform], preflight)


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
            metadata_sha = subprocess.check_output(
                ["git", "-C", repo, "rev-parse", "HEAD"], text=True
            ).strip()
            self.assertNotEqual(source_sha, metadata_sha)
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
            valid = log.read_text(encoding="utf-8")
            log.write_text(valid + "SCRIPT ERROR: broken fixture\n", encoding="utf-8")
            failed = subprocess.run([ROOT / "scripts/check-alpha-smoke-log.sh", log], check=False)
            self.assertNotEqual(failed.returncode, 0)
            log.write_text(
                valid + "WARNING: 1 ObjectDB instance was leaked at exit\n",
                encoding="utf-8",
            )
            failed = subprocess.run([ROOT / "scripts/check-alpha-smoke-log.sh", log], check=False)
            self.assertNotEqual(failed.returncode, 0)
            dummy_shader_error = (
                "ERROR: 1 RID allocations of type "
                "'N13RendererDummy15MaterialStorage11DummyShaderE' were leaked at exit.\n"
            )
            log.write_text(valid + dummy_shader_error, encoding="utf-8")
            failed = subprocess.run([ROOT / "scripts/check-alpha-smoke-log.sh", log], check=False)
            self.assertNotEqual(failed.returncode, 0)
            subprocess.run(
                [
                    ROOT / "scripts/check-alpha-smoke-log.sh",
                    "--allow-macos-dummy-shader-leak",
                    log,
                ],
                check=True,
            )
            dummy_shader_error_two = dummy_shader_error.replace(
                "ERROR: 1 RID allocations", "ERROR: 2 RID allocations"
            )
            log.write_text(valid + dummy_shader_error_two, encoding="utf-8")
            subprocess.run(
                [
                    ROOT / "scripts/check-alpha-smoke-log.sh",
                    "--allow-macos-dummy-shader-leak",
                    log,
                ],
                check=True,
            )
            dummy_shader_error_three = dummy_shader_error.replace(
                "ERROR: 1 RID allocations", "ERROR: 3 RID allocations"
            )
            log.write_text(valid + dummy_shader_error_three, encoding="utf-8")
            failed = subprocess.run(
                [
                    ROOT / "scripts/check-alpha-smoke-log.sh",
                    "--allow-macos-dummy-shader-leak",
                    log,
                ],
                check=False,
            )
            self.assertNotEqual(failed.returncode, 0)
            log.write_text(
                valid + dummy_shader_error + "ERROR: unrelated renderer failure\n",
                encoding="utf-8",
            )
            failed = subprocess.run(
                [
                    ROOT / "scripts/check-alpha-smoke-log.sh",
                    "--allow-macos-dummy-shader-leak",
                    log,
                ],
                check=False,
            )
            self.assertNotEqual(failed.returncode, 0)
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
        self.assertGreaterEqual(
            preflight.count("ref: ${{ needs.metadata.outputs.source_sha }}"), 5
        )
        self.assertGreaterEqual(
            preflight.count('test "$(git rev-parse HEAD)" = "$SOURCE_SHA"'), 4
        )
        self.assertIn(".pull_request.head.sha", preflight)
        # GitHub forbids overriding GITHUB_* default variables, so provenance
        # always binds the workflow commit. Tag runs re-prove that the tag
        # commit adds only release metadata on top of the qualified source and
        # verify attestations against the actually-attested commit.
        self.assertNotIn("GITHUB_SHA: ${{", preflight)
        attest_step = preflight.split("Attest candidate archive provenance", 1)[1]
        attest_step = attest_step.split("with:", 1)[0]
        self.assertNotIn("env:", attest_step)
        self.assertIn('attest_sha="$GITHUB_SHA"', preflight)
        self.assertIn('--source-digest "$attest_sha"', preflight)
        self.assertIn("environment: alpha-release", preflight)
        self.assertIn(
            "if: github.event_name == 'workflow_dispatch' && inputs.candidate_mode == 'trusted-release'",
            preflight,
        )
        self.assertIn("candidate-mode trusted-release", preflight)
        self.assertIn("environment: alpha-release", publisher)
        self.assertIn("refusing to overwrite it", publisher)
        self.assertIn("--expected-sha256", publisher)
        self.assertIn('echo "tag_sha=$tag_sha" >> "$GITHUB_OUTPUT"', publisher)
        self.assertIn("VALIDATED_TAG_SHA: ${{ steps.validate.outputs.tag_sha }}", publisher)
        self.assertIn("VALIDATED_SOURCE_SHA: ${{ steps.validate.outputs.source_sha }}", publisher)
        self.assertNotIn('= "$tag_sha"', publisher.split("Verify and publish release", 1)[1])
        self.assertIn('--source-digest "$VALIDATED_SOURCE_SHA"', publisher)
        self.assertNotIn(".head_branch == $tag", publisher)
        self.assertNotIn('--source-sha "$GITHUB_SHA"', preflight)
        self.assertNotIn('--source-sha "$GITHUB_SHA"', packages)
        self.assertIn("check-release-tag-binding.sh", preflight)
        self.assertIn("check-release-tag-binding.sh", publisher)
        self.assertIn("github.ref == 'refs/heads/main'", packages)
        # Trusted eligibility and publication require qualified source to be
        # contained in the protected main branch; a bare dispatch at an
        # arbitrary ref must never satisfy the evidence chain.
        self.assertIn(
            'git merge-base --is-ancestor "$SOURCE_SHA" refs/remotes/origin/main',
            preflight,
        )
        self.assertIn(
            'git merge-base --is-ancestor "$source_sha" refs/remotes/origin/main',
            publisher,
        )
        self.assertIn(
            'git merge-base --is-ancestor "$VALIDATED_SOURCE_SHA" refs/remotes/origin/main',
            publisher,
        )

    def test_publisher_accepts_exact_arm64_inclusive_trusted_inventory(self):
        publisher = (ROOT / ".github/workflows/client-publish.yml").read_text(encoding="utf-8")
        job_validation = publisher.split("jobs_json=", 1)[1].split("ci_runs_json=", 1)[0]
        artifact_validation = publisher.split("artifacts_json=", 1)[1].split("releases_json=", 1)[0]
        expected_jobs = (
            "Validate client candidate",
            "Linux x86_64 client",
            "Linux ARM64 client",
            "Windows x86_64 client",
            "macOS universal client",
            "Assemble checksummed nonpublishing candidate",
            "Sign and verify Windows release client",
            "Sign, notarize, and verify macOS release client",
            "Validate protected trusted release candidate",
        )
        for name in expected_jobs:
            self.assertEqual(job_validation.count(f'name: "{name}"'), 1, name)
        self.assertIn("(.total_count == 8)", artifact_validation)
        for artifact in (
            "client-linux",
            "client-linux-arm64",
            "client-macos",
            "client-windows",
            "client-macos-trusted",
            "client-windows-trusted",
        ):
            self.assertIn(f'"{artifact}"', artifact_validation)

    def test_trusted_desktop_signing_is_protected_and_fail_closed(self):
        preflight = (ROOT / ".github/workflows/client-release.yml").read_text(encoding="utf-8")
        windows = preflight.split("  trusted-windows:", 1)[1].split("  trusted-macos:", 1)[0]
        macos = preflight.split("  trusted-macos:", 1)[1].split("  trusted-candidate:", 1)[0]
        trusted = preflight.split("  trusted-candidate:", 1)[1]
        for job in (windows, macos):
            self.assertIn("environment: alpha-release", job)
            self.assertIn(
                "if: github.event_name == 'workflow_dispatch' && inputs.candidate_mode == 'trusted-release'",
                job,
            )
        self.assertIn("SPURFIRE_WINDOWS_PFX_BASE64", windows)
        self.assertIn("Set-AuthenticodeSignature", windows)
        self.assertIn("TimeStamperCertificate", windows)
        self.assertIn("SPURFIRE_APPLE_CERTIFICATE_P12_BASE64", macos)
        self.assertIn("xcrun notarytool submit", macos)
        self.assertIn("xcrun stapler validate", macos)
        self.assertIn("spctl --assess", macos)
        self.assertIn("client-windows-trusted", trusted)
        self.assertIn("client-macos-trusted", trusted)
        self.assertIn("Attest protected signed candidate provenance", trusted)

    def test_desktop_jobs_run_behavioral_native_smoke(self):
        # ABI, loader, input, and exported-method regressions must not ship in
        # desktop artifacts that only launched a bootstrap scene for 30 frames.
        preflight = (ROOT / ".github/workflows/client-release.yml").read_text(encoding="utf-8")
        # Linux x86_64, Linux ARM64, Windows, macOS arm64, and the macOS
        # x86_64 slice under Rosetta all run the full smoke suite.
        self.assertGreaterEqual(preflight.count("scripts/test-godot.sh"), 5)
        self.assertGreaterEqual(preflight.count("scripts/check-alpha-smoke-log.sh"), 5)
        self.assertEqual(preflight.count("--allow-macos-dummy-shader-leak"), 2)
        self.assertIn('GODOT_BIN="$PWD/godot4.exe"', preflight)
        self.assertIn('GODOT_BIN="$PWD/Godot.app/Contents/MacOS/Godot"', preflight)
        # The macOS x86_64 slice must execute, not merely cross-compile.
        self.assertIn("softwareupdate --install-rosetta", preflight)
        self.assertIn("arch -x86_64", preflight)
        self.assertIn("godot-smoke-x86_64.log", preflight)

    def test_candidate_metadata_is_nonpublishing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root)
            output = root / "output"
            result = run_candidate_metadata(source, output)
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((output / "candidate-manifest.json").read_text())
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(manifest["candidate_mode"], "preflight")
            self.assertFalse(manifest["release_eligible"])
            self.assertEqual(manifest["publication"], "forbidden")
            self.assertEqual(len(manifest["artifacts"]), 4)
            self.assertTrue((output / "SHA256SUMS").is_file())
            self.assertTrue((output / "candidate.spdx.json").is_file())

    def trusted_args(self, apple: str = "A" * 10, windows: str = "a" * 64) -> tuple[str, ...]:
        return (
            "--candidate-mode",
            "trusted-release",
            "--approved-apple-team-id",
            apple,
            "--approved-windows-certificate-sha256",
            windows,
        )

    def test_trusted_candidate_eligibility_requires_bound_verified_records(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root, trusted=True)
            output = root / "output"
            result = run_candidate_metadata(source, output, *self.trusted_args())
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((output / "candidate-manifest.json").read_text())
            self.assertTrue(manifest["release_eligible"])
            self.assertFalse(manifest["candidate_only"])
            self.assertEqual(manifest["blockers"], [])
            self.assertEqual(manifest["publication"], "protected_manual_only")

    def test_missing_linux_arm64_archive_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root)
            (source / "Spurfire-linux-arm64.tar.gz").unlink()
            result = run_candidate_metadata(source, root / "output")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected client archives", result.stderr)

    def test_tampered_linux_arm64_trust_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root)
            trust_path = source / "linux-arm64-trust.json"
            trust = json.loads(trust_path.read_text())
            trust["launch_smoke_passed"] = False
            trust_path.write_text(json.dumps(trust), encoding="utf-8")
            result = run_candidate_metadata(source, root / "output")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("linux-arm64", result.stderr)

    def test_tampered_trust_metadata_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root, trusted=True)
            trust_path = source / "windows-trust.json"
            trust = json.loads(trust_path.read_text())
            trust["archive_sha256"] = "f" * 64
            trust_path.write_text(json.dumps(trust), encoding="utf-8")
            result = run_candidate_metadata(
                source, root / "output", *self.trusted_args()
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("archive_sha256 does not match", result.stderr)

    def test_trusted_mode_rejects_unsigned_records(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root)
            output = root / "output"
            result = run_candidate_metadata(source, output, *self.trusted_args())
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((output / "candidate-manifest.json").read_text())
            self.assertFalse(manifest["release_eligible"])
            self.assertIn("verified Apple", " ".join(manifest["blockers"]))

    def test_trusted_mode_rejects_unapproved_signer_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = write_candidate_inputs(root, trusted=True)
            output = root / "output"
            result = run_candidate_metadata(
                source, output, *self.trusted_args("B" * 10, "b" * 64)
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((output / "candidate-manifest.json").read_text())
            self.assertFalse(manifest["release_eligible"])
            self.assertNotEqual(manifest["blockers"], [])

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
