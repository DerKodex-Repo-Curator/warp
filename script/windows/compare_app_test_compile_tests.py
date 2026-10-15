from pathlib import Path
import json
import os
import shutil
import subprocess
import tempfile
import unittest


COLLECTOR = r"""
param($Mode, $Phase, $OutputDirectory)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath 'target/.fingerprint/restored')) {
    throw 'Warm cache fingerprint was lost.'
}
if (Test-Path -LiteralPath 'target/compiled-marker') {
    throw 'A previous compile leaked into this sample.'
}
$variant = (Get-Content -LiteralPath 'crates/warpui_core/src/core/app.rs' -Raw).Trim()
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
@{ fixture_variant = $variant } | ConvertTo-Json |
    Set-Content -LiteralPath (Join-Path $OutputDirectory 'metadata.json')
Set-Content -LiteralPath 'target/compiled-marker' -Value $variant
if ((Split-Path $OutputDirectory -Leaf) -eq $env:FAIL_PHASE) {
    exit 9
}
exit 0
"""


@unittest.skipUnless(shutil.which("pwsh") and shutil.which("git"), "requires PowerShell and git")
class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="warp-compile-comparison-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.sources = [
            self.repo / "crates/warpui_core/src/core/app.rs",
            self.repo / "crates/warpui_core/src/core/mod_tests.rs",
        ]
        scripts = self.repo / "script/windows"
        scripts.mkdir(parents=True)
        self.script = scripts / "compare_app_test_compile.ps1"
        shutil.copyfile(Path(__file__).with_name(self.script.name), self.script)
        (scripts / "measure_ci_performance.ps1").write_text(COLLECTOR)
        self.git("init", "-q")
        for source in self.sources:
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text("baseline\n")
        self.commit("baseline fixture")
        self.baseline = self.git("rev-parse", "HEAD").stdout.strip()
        for source in self.sources:
            source.write_text("candidate\n")
        self.commit("candidate fixture")
        fingerprint = self.repo / "target/.fingerprint/restored"
        fingerprint.parent.mkdir(parents=True)
        fingerprint.write_text("warm cache\n")
        self.runner = self.root / "runner"
        self.runner.mkdir()
        self.env = os.environ.copy()
        for key in (
            "CARGO_TARGET_DIR",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTC_BOOTSTRAP",
            "CARGO_ENCODED_RUSTFLAGS",
            "FAIL_PHASE",
        ):
            self.env.pop(key, None)
        self.env.update(
            GITHUB_ACTIONS="true",
            RUNNER_OS="Windows",
            GITHUB_WORKSPACE=str(self.repo),
            RUNNER_TEMP=str(self.runner),
            GITHUB_RUN_ID="fixture",
            GITHUB_RUN_ATTEMPT="1",
            CARGO_INCREMENTAL="0",
            RUSTFLAGS="-C debuginfo=line-tables-only --cfg=web_sys_unstable_apis",
        )

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.repo, capture_output=True, text=True, check=True
        )

    def commit(self, message):
        self.git("add", ".")
        self.git(
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            message + "\n\nCo-Authored-By: Warp <agent@warp.dev>",
        )

    def compare(self):
        return subprocess.run(
            ["pwsh", "-NoProfile", "-File", str(self.script), "-Baseline", self.baseline],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
        )

    def assert_candidate_restored(self):
        for source in self.sources:
            self.assertEqual(source.read_text(), "candidate\n")
        self.assertFalse((self.runner / "warp-app-test-cache-fixture-1").exists())

    def test_balanced_source_order_and_identical_cache_per_sample(self):
        result = self.compare()
        self.assertEqual(result.returncode, 0, result.stderr)
        root = self.runner / "warp-windows-ci-fixture-1"
        for phase, variant in (
            ("compile-candidate-1", "candidate"),
            ("compile-baseline-1", "baseline"),
            ("compile-baseline-2", "baseline"),
            ("compile", "candidate"),
        ):
            metadata = json.loads((root / phase / "metadata.json").read_text())
            self.assertEqual(metadata["fixture_variant"], variant)
            self.assertEqual(metadata["comparison"]["variant"], variant)
        self.assertEqual((self.repo / "target/compiled-marker").read_text().strip(), "candidate")
        self.assert_candidate_restored()

    def test_failed_baseline_compile_still_restores_candidate_sources(self):
        self.env["FAIL_PHASE"] = "compile-baseline-1"
        result = self.compare()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Comparison compilation failed", result.stderr)
        self.assert_candidate_restored()

    def test_non_ci_invocation_leaves_the_target_untouched(self):
        self.env["GITHUB_ACTIONS"] = "false"
        result = self.compare()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("restricted", result.stderr)
        self.assertEqual((self.repo / "target/.fingerprint/restored").read_text(), "warm cache\n")
        self.assert_candidate_restored()


if __name__ == "__main__":
    unittest.main()
