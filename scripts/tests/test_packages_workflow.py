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
        trust_partition = (
            "${{ github.event_name == 'push' && github.ref == "
            "'refs/heads/main' && 'trusted' || 'untrusted' }}"
        )
        cache_scope = (
            "scope=spurfire-server-validate-${{ matrix.arch }}-" + trust_partition
        )
        self.assertIn(f"cache-from: type=gha,{cache_scope}", job)
        self.assertIn(f"cache-to: type=gha,mode=min,{cache_scope}", job)
        self.assertNotIn("packages: write", job)
        self.assertNotIn("docker/login-action", job)

    def test_native_publish_is_by_digest_with_unique_artifacts(self):
        job = self.job("publish-image", "publish")
        self.assertIn(
            "if: github.event_name == 'push' && github.ref == "
            "'refs/heads/main' && github.repository == 'rajsinghtech/spurfire'",
            job,
        )
        self.assertIn("needs: [metadata, validate, container]", job)
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
        for binding in (
            "test \"$(jq -er '.source_sha' \"$record\")\" = \"$GITHUB_SHA\"",
            "test \"$(jq -er '.arch' \"$record\")\" = \"$arch\"",
            "test \"$(jq -er '.image' \"$record\")\" = \"$IMAGE\"",
            "test \"$(jq -er '.run_id' \"$record\")\" = \"$GITHUB_RUN_ID\"",
            "test \"$(jq -er '.run_attempt' \"$record\")\" = \"$GITHUB_RUN_ATTEMPT\"",
        ):
            self.assertIn(binding, job)
        self.assertIn("docker buildx imagetools create", job)
        self.assertIn('"$IMAGE@${digests[amd64]}"', job)
        self.assertIn('"$IMAGE@${digests[arm64]}"', job)
        self.assertIn("in-toto.io/predicate-type", job)
        self.assertIn("https://spdx.dev/Document", job)
        self.assertIn("https://slsa.dev/provenance/", job)
        self.assertIn('cosign sign --yes "$IMAGE@$IMAGE_DIGEST"', job)
        self.assertIn("subject-digest: ${{ steps.image.outputs.digest }}", job)

    def test_finalizer_rejects_stale_main_before_registry_login(self):
        job = self.job("publish")
        stale_check = job.index(
            'git ls-remote --exit-code "https://github.com/${GITHUB_REPOSITORY}.git" '
            "refs/heads/main"
        )
        comparison = job.index('test "$current_main" = "$GITHUB_SHA"')
        registry_login = job.index("uses: docker/login-action@")
        index_write = job.index("docker buildx imagetools create")
        chart_push = job.index('helm push "$package" "$CHART_REGISTRY"')
        self.assertLess(stale_check, comparison)
        self.assertLess(comparison, registry_login)
        self.assertLess(comparison, index_write)
        self.assertLess(comparison, chart_push)

    def test_every_checkout_is_explicitly_sha_bound(self):
        checkout_steps = re.findall(
            r"(?ms)^      - name: [^\n]+\n"
            r"        uses: actions/checkout@[0-9a-f]{40}[^\n]*\n"
            r"        with:\n"
            r"(?P<with>(?:          [^\n]+\n)+)",
            self.text,
        )
        self.assertTrue(checkout_steps)
        self.assertEqual(
            len(checkout_steps), self.text.count("uses: actions/checkout@")
        )
        for checkout_with in checkout_steps:
            with self.subTest(checkout_with=checkout_with):
                self.assertIn("ref: ${{ github.sha }}", checkout_with)
                self.assertIn("persist-credentials: false", checkout_with)

        exact_source_assertions = self.text.count(
            'test "$(git rev-parse HEAD)" = "$GITHUB_SHA"'
        )
        self.assertEqual(exact_source_assertions, len(checkout_steps))

    def test_all_actions_are_pinned_to_full_commit(self):
        uses = re.findall(r"^\s*-?\s*uses:\s*([^\s#]+)", self.text, re.MULTILINE)
        self.assertTrue(uses)
        for action in uses:
            with self.subTest(action=action):
                self.assertRegex(action, r"^[^@]+@[0-9a-f]{40}$")


if __name__ == "__main__":
    unittest.main()
