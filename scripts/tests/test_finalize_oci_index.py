#!/usr/bin/env python3
"""Regression tests for deterministic OCI index finalization."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "finalize-oci-index.sh"
FIXTURE = Path(__file__).parent / "fixtures" / "oci-index.json"
IMAGE = "ghcr.io/rajsinghtech/spurfire-server"
AMD64 = "sha256:db9425ec015b99d23916a7543e62db5dd1e4b3a1507c0bc39b517c15ed85e88b"
ARM64 = "sha256:837ec8083db53f62d516d3b08731772fedfad43476d86f823bc94b62d925f31c"
INDEX = "sha256:87a3f3e2b3036112c017b8477092eca0c2312cb8b691455f8ff6982b03f0a1d3"


class FinalizeOciIndexTests(unittest.TestCase):
    def run_helper(self, fixture: Path, *args: str) -> tuple[subprocess.CompletedProcess[str], list[list[str]]]:
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            log = temp / "docker-calls.jsonl"
            docker = temp / "docker"
            docker.write_text(
                """#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
with Path(os.environ["DOCKER_CALL_LOG"]).open("a", encoding="utf-8") as log:
    log.write(json.dumps(args) + "\\n")
if args[:4] == ["buildx", "imagetools", "create", "--dry-run"]:
    sys.stdout.buffer.write(Path(os.environ["INDEX_FIXTURE"]).read_bytes())
elif args[:3] != ["buildx", "imagetools", "create"]:
    sys.exit(64)
""",
                encoding="utf-8",
            )
            docker.chmod(0o755)
            env = os.environ.copy()
            env.update(
                PATH=f"{temp}:{env['PATH']}",
                DOCKER_CALL_LOG=str(log),
                INDEX_FIXTURE=str(fixture),
            )
            result = subprocess.run(
                [str(SCRIPT), *args],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, calls

    def test_exact_dry_run_bytes_produce_pushed_digest_without_tag_inspection(self):
        self.assertFalse(FIXTURE.read_bytes().endswith(b"\n"))
        result, calls = self.run_helper(
            FIXTURE,
            IMAGE,
            AMD64,
            ARM64,
            f"{IMAGE}:main",
            f"{IMAGE}:sha-eeb3da27a2a598f798a1ff010e9cd4c5f4e5ea4d",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{INDEX}\n")
        refs = [f"{IMAGE}@{AMD64}", f"{IMAGE}@{ARM64}"]
        self.assertEqual(calls[0], ["buildx", "imagetools", "create", "--dry-run", *refs])
        self.assertEqual(
            calls[1],
            [
                "buildx",
                "imagetools",
                "create",
                "--tag",
                f"{IMAGE}:main",
                "--tag",
                f"{IMAGE}:sha-eeb3da27a2a598f798a1ff010e9cd4c5f4e5ea4d",
                *refs,
            ],
        )
        self.assertEqual(len(calls), 2)
        self.assertNotIn("inspect", result.stderr)

    def test_malformed_platform_composition_is_rejected_before_push(self):
        with tempfile.TemporaryDirectory() as directory:
            malformed = Path(directory) / "index.json"
            data = json.loads(FIXTURE.read_text(encoding="utf-8"))
            data["manifests"][2]["platform"]["architecture"] = "amd64"
            malformed.write_text(json.dumps(data, separators=(",", ":")), encoding="utf-8")
            result, calls = self.run_helper(
                malformed, IMAGE, AMD64, ARM64, f"{IMAGE}:main"
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("--dry-run", calls[0])

    def test_unlinked_attestation_is_rejected_before_push(self):
        with tempfile.TemporaryDirectory() as directory:
            malformed = Path(directory) / "index.json"
            data = json.loads(FIXTURE.read_text(encoding="utf-8"))
            data["manifests"][1]["annotations"]["vnd.docker.reference.digest"] = (
                f"sha256:{'f' * 64}"
            )
            malformed.write_text(json.dumps(data, separators=(",", ":")), encoding="utf-8")
            result, calls = self.run_helper(
                malformed, IMAGE, AMD64, ARM64, f"{IMAGE}:main"
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)

    def test_malformed_digest_and_tag_are_rejected_without_docker(self):
        cases = (
            (IMAGE, "sha256:bad", ARM64, f"{IMAGE}:main"),
            (IMAGE, AMD64, ARM64, "ghcr.io/other/project:main"),
            (IMAGE, AMD64, ARM64, f"{IMAGE}@sha256:{'0' * 64}"),
        )
        for args in cases:
            with self.subTest(args=args):
                result, calls = self.run_helper(FIXTURE, *args)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
