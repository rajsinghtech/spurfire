#!/usr/bin/env python3
"""Static fail-closed checks for native container publication."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "packages.yml"


class PackagesWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = WORKFLOW.read_text(encoding="utf-8")

    def job(self, name: str, next_name: str | None = None) -> str:
        start = self.text.index(f"  {name}:\n")
        if next_name is None:
            return self.text[start:]
        return self.text[start : self.text.index(f"  {next_name}:\n", start)]

    def test_qemu_and_forced_runtime_platform_are_forbidden(self):
        self.assertNotIn("setup-qemu", self.text)
        self.assertNotRegex(self.text, r"docker run(?:.|\n)*?--platform")

    def test_validation_uses_native_include_matrix_and_min_caches(self):
        job = self.job("container", "publish-image")
        self.assertIn("runs-on: ${{ matrix.runner }}", job)
        self.assertRegex(
            job,
            r"include:\s+"
            r"- arch: amd64\s+runner: ubuntu-24\.04\s+machine: x86_64\s+"
            r"- arch: arm64\s+runner: ubuntu-24\.04-arm\s+machine: aarch64",
        )
        self.assertIn("test \"$(uname -m)\" = \"$EXPECTED_MACHINE\"", job)
        self.assertIn("test \"$(docker image inspect --format '{{.Architecture}}'", job)
        self.assertIn("platforms: linux/${{ matrix.arch }}", job)
        self.assertIn("load: true", job)
        self.assertIn("push: false", job)
        self.assertRegex(
            job,
            r"cache-to: type=gha,mode=min,scope=spurfire-server-validate-"
            r"\$\{\{ matrix\.arch \}\}",
        )
        self.assertNotIn("packages: write", job)
        self.assertNotIn("docker/login-action", job)

    def test_native_publish_is_by_digest_with_unique_artifacts(self):
        job = self.job("publish-image", "publish")
        self.assertIn("runs-on: ${{ matrix.runner }}", job)
        self.assertIn("runner: ubuntu-24.04-arm", job)
        self.assertIn("platforms: linux/${{ matrix.arch }}", job)
        self.assertNotIn("linux/amd64,linux/arm64", job)
        self.assertIn("push-by-digest=true", job)
        self.assertIn("name: image-digest-${{ matrix.arch }}", job)
        self.assertIn("path: digest-${{ matrix.arch }}.json", job)
        self.assertIn("sbom: true", job)
        self.assertIn("provenance: mode=max", job)
        self.assertIn("scope=spurfire-server-publish-${{ matrix.arch }}", job)
        self.assertNotRegex(job, r"scope=spurfire-server-publish(?:\s|$)")

    def test_finalizer_requires_both_digest_artifacts_and_exact_index(self):
        job = self.job("publish")
        self.assertIn("needs: [metadata, publish-image]", job)
        self.assertIn("runs-on: ubuntu-24.04", job)
        for permission in (
            "actions: read",
            "contents: read",
            "packages: write",
            "id-token: write",
            "attestations: write",
        ):
            self.assertIn(permission, job)
        self.assertIn("pattern: image-digest-*", job)
        self.assertIn("test \"${records[0]}\" = digest-amd64.json", job)
        self.assertIn("test \"${records[1]}\" = digest-arm64.json", job)
        self.assertIn("docker buildx imagetools create", job)
        self.assertIn('"$IMAGE@${digests[amd64]}"', job)
        self.assertIn('"$IMAGE@${digests[arm64]}"', job)
        self.assertIn("in-toto.io/predicate-type", job)
        self.assertIn("https://spdx.dev/Document", job)
        self.assertIn("https://slsa.dev/provenance/", job)
        self.assertIn('cosign sign --yes "$IMAGE@$IMAGE_DIGEST"', job)
        self.assertIn("subject-digest: ${{ steps.image.outputs.digest }}", job)

    def test_every_checkout_is_explicitly_sha_bound(self):
        checkouts = self.text.count("uses: actions/checkout@")
        sha_refs = self.text.count("ref: ${{ github.sha }}")
        exact_source_assertions = self.text.count(
            'test "$(git rev-parse HEAD)" = "$GITHUB_SHA"'
        )
        self.assertEqual(checkouts, 5)
        self.assertEqual(sha_refs, checkouts)
        self.assertEqual(exact_source_assertions, checkouts)

    def test_all_actions_are_pinned_to_full_commit(self):
        uses = re.findall(r"^\s*-?\s*uses:\s*([^\s#]+)", self.text, re.MULTILINE)
        self.assertTrue(uses)
        for action in uses:
            with self.subTest(action=action):
                self.assertRegex(action, r"^[^@]+@[0-9a-f]{40}$")


if __name__ == "__main__":
    unittest.main()
