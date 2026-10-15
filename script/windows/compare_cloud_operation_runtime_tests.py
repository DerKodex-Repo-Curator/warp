"""Disposable native-command fixtures; never build the application or use its target."""

import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("runtime_comparison", HERE / "runtime_comparison.py")
runtime = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runtime)
PWSH = shutil.which("pwsh")
SENTINEL = "fixture-private-output-not-for-publication"
FAKE_COLLECTOR = r"""
param($Mode, $OutputDirectory, $Phase)
$ErrorActionPreference = 'Stop'
switch ($Mode) {
    'Start' {
        New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
        Set-Content -LiteralPath (Join-Path $OutputDirectory 'metadata.json') -Value '{"logical_processors":2}'
    }
    'Collect' {
        New-Item -ItemType File -Path (Join-Path $OutputDirectory 'entered') | Out-Null
        Set-Content -LiteralPath (Join-Path $OutputDirectory 'samples.jsonl') -Value '{"cpu_percent":1}'
        if ($env:FIXTURE_FAIL -eq 'collector' -and (Split-Path $OutputDirectory -Leaf).StartsWith('family-')) {
            Set-Content -LiteralPath (Join-Path $OutputDirectory 'errors.jsonl') -Value '{"stage":"fixture"}'
        }
        while (-not (Test-Path -LiteralPath (Join-Path $OutputDirectory 'stop'))) {
            Start-Sleep -Milliseconds 10
        }
        New-Item -ItemType File -Path (Join-Path $OutputDirectory 'completed') | Out-Null
    }
    'Stop' { New-Item -ItemType File -Path (Join-Path $OutputDirectory 'stop') | Out-Null }
}
"""

FAKE_NATIVE = r"""
import json, os, pathlib, sys, xml.etree.ElementTree as ET
root = pathlib.Path.cwd()
args = sys.argv[1:]
tool = pathlib.Path(sys.argv[0]).name
runner = pathlib.Path(os.environ["RUNNER_TEMP"])
source = root / "app/src/server/cloud_objects/update_manager_tests.rs"
target = root / "target"
binary = target / "debug/deps/warp-fixture.exe"
prefix = "server::cloud_objects::update_manager::tests::"
added = prefix + "operation_failure_subscription_retains_completion_before_await"
variant = source.read_text().strip()
fail = os.environ.get("FIXTURE_FAIL", "")

def event(kind):
    with (runner / "events.jsonl").open("a") as stream:
        stream.write(json.dumps({"kind": kind, "variant": variant}) + "\n")

def roster():
    names = [prefix + "case_" + str(i) for i in range(73)] + ["outside", "ignored"]
    if variant == "candidate":
        names.append(added)
    if fail == "roster" and variant == "candidate":
        names.remove(prefix + "case_0")
    expression = args[args.index("-E") + 1] if "-E" in args else None
    cases = {}
    for name in names:
        match = {"status": "matches"}
        if name == "ignored":
            match = {"status": "mismatch", "reason": "ignored"}
        elif expression and expression.startswith("test(") and not name.startswith(prefix):
            match = {"status": "mismatch", "reason": "expression"}
        cases[name] = {"kind": "test", "ignored": name == "ignored", "filter-match": match}
    return {"test-count": len(cases), "rust-suites": {
        "warp": {"status": "listed", "testcases": cases},
        "integration": {"status": "skipped", "testcases": {}},
    }}

def assert_prebuilt():
    assert "--workspace" not in args and "--locked" not in args
    assert "-E" in args, "unfiltered prebuilt discovery/execution is outside the experiment scope"
    assert binary.read_text() == variant
    assert (target / "debug/.fingerprint/restored").read_text() == variant
    assert (target / "debug/OpenConsole.exe").read_text() == variant
    assert (target / "debug/profile-root.dll").read_text() == variant
    assert (target / "debug/build/private/out/asset").read_text() == variant

if tool == "rustc":
    print("rustc 1.92.0 (fixture)")
elif args == ["--version"]:
    print("cargo 1.92.0 (fixture)")
elif args == ["nextest", "--version"]:
    print("cargo-nextest 0.9.143 (fixture)")
elif args[0] == "metadata":
    print(json.dumps({"workspace_root": str(root), "target_directory": str(target), "packages": []}))
elif "--no-run" in args:
    event("build")
    print("fixture-private-output-not-for-publication")
    if fail == "build-" + variant:
        sys.exit(9)
    for relative in ["debug/deps/warp-fixture.exe", "debug/.fingerprint/restored",
                     "debug/OpenConsole.exe", "debug/profile-root.dll",
                     "debug/build/private/out/asset"]:
        path = target / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(variant)
    stale = target / "nextest/ci/junit.xml"
    stale.parent.mkdir(parents=True, exist_ok=True)
    stale.write_text("stale-junit-must-be-removed")
elif "--list-type" in args:
    print(json.dumps({
        "rust-build-meta": {"target-directory": str(target), "build-directory": str(target)},
        "rust-binaries": {"warp": {"binary-id": "warp", "kind": "lib", "binary-path": str(binary)}},
    }))
elif "list" in args:
    assert_prebuilt()
    event("list")
    print(json.dumps(roster()))
elif "run" in args:
    assert_prebuilt()
    event("run")
    junit = target / "nextest/ci/junit.xml"
    assert not junit.exists(), "stale JUnit survived"
    print("fixture-private-output-not-for-publication")
    if fail == "missing-junit":
        sys.exit(0)
    cases = {n: c for n, c in roster()["rust-suites"]["warp"]["testcases"].items()
             if c["filter-match"]["status"] == "matches"}
    failed = fail == "sample"
    tree = ET.Element("testsuites", tests=str(len(cases)), skipped="0",
                      failures="1" if failed else "0", errors="0", time="0.5")
    suite = ET.SubElement(tree, "testsuite", name="warp")
    for index, name in enumerate(cases):
        case = ET.SubElement(suite, "testcase", classname="warp", name=name, time="0.2")
        if index == 0 and failed:
            ET.SubElement(case, "failure").text = "fixture-private-output-not-for-publication"
        ET.SubElement(case, "system-out").text = "fixture-private-output-not-for-publication"
    junit.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(tree).write(junit)
    if fail == "binary-change":
        binary.write_text("replacement-binary")
    sys.exit(7 if failed else 0)
else:
    raise AssertionError("unexpected fixture invocation")
"""


class Fixture:
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="warp-runtime-driver-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.runner = self.root / "runner"
        self.runner.mkdir()
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.source = self.repo / runtime.SOURCE
        self.source.parent.mkdir(parents=True)
        scripts = self.repo / "script/windows"
        scripts.mkdir(parents=True)
        self.entry = scripts / "compare_cloud_operation_runtime.ps1"
        for name in ("compare_cloud_operation_runtime.ps1", "runtime_comparison.py"):
            shutil.copy2(HERE / name, scripts / name)
        (scripts / "measure_ci_performance.ps1").write_text(FAKE_COLLECTOR)
        (self.repo / ".gitignore").write_text("target/\n__pycache__/\n")
        self.git("init", "-q")
        self.source.write_text("baseline\n")
        self.commit("baseline fixture")
        self.baseline = self.git("rev-parse", "HEAD").stdout.strip()
        self.source.write_text("candidate\n")
        self.commit("candidate fixture")
        self.candidate = self.git("rev-parse", "HEAD").stdout.strip()
        for tool in ("cargo", "rustc"):
            self.make_tool(tool, FAKE_NATIVE)
        self.make_tool(
            "python", "import os, sys\nos.execv(sys.executable, [sys.executable, *sys.argv[1:]])\n"
        )
        self.env = os.environ.copy()
        for key in list(self.env):
            if key.startswith(("CARGO_", "RUST", "NEXTEST_", "GITHUB_")):
                self.env.pop(key)
        self.env.update(
            PATH=str(self.tools) + os.pathsep + os.environ["PATH"],
            GITHUB_ACTIONS="true",
            RUNNER_OS="Windows",
            GITHUB_WORKSPACE=str(self.repo),
            RUNNER_TEMP=str(self.runner),
            GITHUB_RUN_ID="123",
            GITHUB_RUN_ATTEMPT="1",
            CARGO_INCREMENTAL="0",
            RUSTFLAGS=runtime.NORMAL_FLAGS,
            NEXTEST_PROFILE="ci",
            WORKSPACE_TEST_ARGS="--workspace --locked",
        )
        self.private = self.runner / "warp-cloud-operation-runtime-123-1"
        self.public = self.runner / "warp-windows-ci-123-1"

    def make_tool(self, name, text):
        path = self.tools / name
        path.write_text(f"#!{sys.executable}\n" + text)
        path.chmod(0o755)

    def git(self, *args):
        return subprocess.run(
            ["git", "--no-pager", *args],
            cwd=self.repo,
            capture_output=True,
            text=True,
            check=True,
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

    def invoke(self, mode):
        return subprocess.run(
            [
                PWSH,
                "-NoProfile",
                "-File",
                str(self.entry),
                "-Mode",
                mode,
                "-Baseline",
                self.baseline,
            ],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
        )

    def prepare(self):
        result = self.invoke("Prepare")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.source.read_text(), "candidate\n")


@unittest.skipUnless(PWSH and shutil.which("git"), "requires pwsh and git")
class OrchestrationTests(Fixture, unittest.TestCase):
    def test_complete_balanced_run_has_private_snapshots_and_no_compile_during_measure(self):
        self.prepare()
        result = self.invoke("Measure")
        self.assertEqual(result.returncode, 0, result.stderr)
        events = [
            json.loads(line) for line in (self.runner / "events.jsonl").read_text().splitlines()
        ]
        self.assertEqual(
            [e["variant"] for e in events if e["kind"] == "build"], ["baseline", "candidate"]
        )
        self.assertEqual(
            [e["variant"] for e in events if e["kind"] == "run"],
            list(runtime.ORDER) * 2,
        )
        self.assertEqual(len([e for e in events if e["kind"] == "list"]), 4)
        report = runtime.read_json(self.public / "runtime-comparison.json")
        self.assertTrue(report["complete"])
        for scopes in report["coverage"].values():
            self.assertEqual(set(scopes), set(runtime.FILTERS))
        self.assertEqual(len(report["samples"]), 8)
        self.assertEqual(report["samples"][0]["nextest_wall_seconds"], 0.5)
        self.assertGreater(report["samples"][0]["summed_test_elapsed_seconds"], 0.5)
        self.assertIsNone(report["samples"][0]["recorded_retry_failures"])
        self.assertEqual(self.source.read_text(), "candidate\n")
        self.assertEqual((self.repo / "target/debug/profile-root.dll").read_text(), "candidate")
        for path in self.public.rglob("*"):
            if path.is_file():
                self.assertNotIn(SENTINEL, path.read_text())
                self.assertNotIn(path.suffix, (".xml", ".exe", ".dll"))
                self.assertNotIn(path.name, ("cargo.json", "binaries.json"))
        self.assertIn(SENTINEL, (self.private / "family-1-candidate.stdout").read_text())

    def test_failed_baseline_build_restores_candidate_and_publishes_only_failure_code(self):
        self.env["FIXTURE_FAIL"] = "build-baseline"
        result = self.invoke("Prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.source.read_text(), "candidate\n")
        report = runtime.read_json(self.public / "runtime-comparison.json")
        self.assertEqual(report["issue"], "prepare-build-failed")
        self.assertFalse((self.private / "state.json").exists())

    def test_failed_sample_retains_report_and_exit_code_and_does_not_retry(self):
        self.prepare()
        self.env["FIXTURE_FAIL"] = "sample"
        result = self.invoke("Measure")
        self.assertNotEqual(result.returncode, 0)
        report = runtime.read_json(self.public / "runtime-comparison.json")
        self.assertEqual(report["issue"], "test-process-failed")
        self.assertEqual(len(report["samples"]), 1)
        self.assertEqual(report["samples"][0]["failures"], 1)
        self.assertEqual(report["samples"][0]["exit_code"], 7)
        self.assertTrue((self.private / "family-1-candidate.junit.xml").exists())
        self.assertNotIn(SENTINEL, json.dumps(report))
        self.assertEqual(self.source.read_text(), "candidate\n")

    def test_missing_report_changed_binary_roster_and_collector_fail_closed(self):
        self.prepare()
        pristine = runtime.read_json(self.private / "state.json")
        for mode, issue in (
            ("missing-junit", "missing-junit"),
            ("binary-change", "binary-changed-during-run"),
            ("roster", "changed-test-roster"),
            ("collector", "sample-collector-incomplete"),
        ):
            with self.subTest(mode=mode):
                (self.private / "measure-started").unlink(missing_ok=True)
                for label in ("family-1-candidate",):
                    shutil.rmtree(self.public / label, ignore_errors=True)
                runtime.write_json(self.private / "state.json", pristine)
                self.env["FIXTURE_FAIL"] = mode
                result = self.invoke("Measure")
                self.assertNotEqual(result.returncode, 0)
                report = runtime.read_json(self.public / "runtime-comparison.json")
                self.assertEqual(report["issue"], issue)
                self.assertFalse(report["complete"])
                self.assertEqual(self.source.read_text(), "candidate\n")
                self.assertEqual(
                    (self.repo / "target/debug/deps/warp-fixture.exe").read_text(), "candidate"
                )

    def test_existing_state_is_not_overwritten(self):
        self.private.mkdir()
        self.public.mkdir()
        report = self.public / "runtime-comparison.json"
        report.write_text('{"preexisting":true}\n')
        result = self.invoke("Prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report.read_text(), '{"preexisting":true}\n')
        self.assertEqual(self.source.read_text(), "candidate\n")

    def test_source_hash_guard_precedes_any_timestamp_restoration(self):
        self.prepare()
        driver = runtime.RuntimeComparison(self.repo, "Measure", self.baseline)
        driver.candidate = self.candidate
        driver.private = self.private
        driver.state = runtime.read_json(self.private / "state.json")
        with (
            mock.patch.object(
                driver,
                "restore_source",
                side_effect=lambda _: self.source.write_text("unexpected edit"),
            ),
            mock.patch.object(runtime.os, "utime") as restore_timestamp,
        ):
            with self.assertRaisesRegex(runtime.ComparisonError, "source-provenance-mismatch"):
                driver.restore_variant("baseline")
            restore_timestamp.assert_not_called()
        self.git("restore", "--worktree", "--", runtime.SOURCE)

    def test_invalid_environment_and_build_scope_do_not_touch_target(self):
        marker = self.repo / "target/owned-marker"
        marker.parent.mkdir()
        marker.write_text("untouched")
        for key, value in (
            ("GITHUB_ACTIONS", "false"),
            ("CARGO_INCREMENTAL", "1"),
            ("CARGO_TARGET_DIR", str(self.root / "elsewhere")),
            ("RUSTC_BOOTSTRAP", "1"),
            ("NEXTEST_TEST_THREADS", "1"),
            ("NEXTEST_PROFILE", "default"),
        ):
            with self.subTest(key=key):
                self.env[key] = value
                result = self.invoke("Prepare")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(marker.read_text(), "untouched")
                self.env.pop(key)
                self.env.update(CARGO_INCREMENTAL="0", GITHUB_ACTIONS="true", NEXTEST_PROFILE="ci")
        config = self.repo / ".config/nextest.toml"
        config.parent.mkdir()
        config.write_text("[profile.ci]\nretries = 0\n")
        self.commit("unexpected configuration change")
        result = self.invoke("Prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected-source-scope", result.stderr)
        self.assertEqual(marker.read_text(), "untouched")

    def test_symlink_target_is_rejected_without_following_it(self):
        elsewhere = self.root / "elsewhere"
        elsewhere.mkdir()
        marker = elsewhere / "marker"
        marker.write_text("untouched")
        (self.repo / "target").symlink_to(elsewhere, target_is_directory=True)
        result = self.invoke("Prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(marker.read_text(), "untouched")
        self.assertFalse(self.private.exists())


class EvidenceTests(unittest.TestCase):
    def roster(self, candidate=False):
        cases = {
            runtime.FAMILY_PREFIX + "one": {
                "kind": "test",
                "ignored": False,
                "filter-match": {"status": "matches"},
            },
            "ignored": {
                "kind": "test",
                "ignored": True,
                "filter-match": {"status": "mismatch", "reason": "ignored"},
            },
        }
        if candidate:
            cases[runtime.ADDED_TEST] = copy.deepcopy(cases[runtime.FAMILY_PREFIX + "one"])
        return {
            "test-count": len(cases),
            "rust-suites": {"warp": {"status": "listed", "testcases": cases}},
        }

    def test_roster_rejects_removed_ignored_filter_changes_and_unknown_additions(self):
        baseline = runtime.parse_roster(self.roster())
        candidate = self.roster(True)
        runtime.compare_rosters(baseline, runtime.parse_roster(candidate))
        for mutation in ("ignored", "filter", "remove", "extra"):
            changed = copy.deepcopy(candidate)
            cases = changed["rust-suites"]["warp"]["testcases"]
            if mutation == "ignored":
                cases["ignored"]["ignored"] = False
            elif mutation == "filter":
                cases[runtime.FAMILY_PREFIX + "one"]["filter-match"] = {"status": "mismatch"}
            elif mutation == "remove":
                cases.pop("ignored")
                changed["test-count"] -= 1
            else:
                cases["unexpected"] = copy.deepcopy(cases[runtime.ADDED_TEST])
                changed["test-count"] += 1
            with self.subTest(mutation=mutation), self.assertRaises(runtime.ComparisonError):
                runtime.compare_rosters(baseline, runtime.parse_roster(changed))

    def test_junit_uses_root_wall_and_omits_arbitrary_payloads_and_recognizes_retry_nodes(self):
        roster = runtime.parse_roster(self.roster())
        with tempfile.TemporaryDirectory(prefix="warp-junit-summary-") as temporary:
            path = Path(temporary) / "junit.xml"
            path.write_text(
                f'<testsuites tests="1" skipped="0" failures="0" errors="0" time="0.5">'
                f'<testsuite><testcase classname="warp" name="{runtime.FAMILY_PREFIX}one" time="2">'
                f'<flakyFailure message="{SENTINEL}">{SENTINEL}</flakyFailure>'
                f"<system-out>{SENTINEL}</system-out></testcase></testsuite></testsuites>"
            )
            result = runtime.parse_junit(path, roster)
            self.assertEqual(result["nextest_wall_seconds"], 0.5)
            self.assertEqual(result["summed_test_elapsed_seconds"], 2)
            self.assertEqual(result["recorded_retry_failures"], 1)
            self.assertNotIn(SENTINEL, json.dumps(result))
            path.write_text(path.read_text().replace('time="2"', 'time="NaN"'))
            with self.assertRaises(runtime.ComparisonError):
                runtime.parse_junit(path, roster)

    def test_snapshot_rejects_existing_symlink_entries_and_insufficient_space(self):
        with tempfile.TemporaryDirectory(prefix="warp-target-snapshot-") as temporary:
            root = Path(temporary).resolve()
            target = root / "target"
            target.mkdir()
            (target / ".hidden").write_text("fingerprint")
            snapshot = root / "snapshot"
            with mock.patch.object(runtime.shutil, "disk_usage", return_value=mock.Mock(free=1)):
                with self.assertRaisesRegex(runtime.ComparisonError, "insufficient-snapshot-space"):
                    runtime.snapshot_target(target, snapshot)
            (target / "link").symlink_to(root / "outside")
            with self.assertRaisesRegex(runtime.ComparisonError, "redirected-target-entry"):
                runtime.snapshot_target(target, snapshot)
            (target / "link").unlink()
            checksum = runtime.snapshot_target(target, snapshot)
            self.assertEqual((snapshot / ".hidden").read_text(), "fingerprint")
            with self.assertRaisesRegex(runtime.ComparisonError, "existing-snapshot"):
                runtime.snapshot_target(target, snapshot)
            (target / ".hidden").write_text("changed")
            runtime.restore_target(snapshot, target, checksum)
            self.assertEqual((target / ".hidden").read_text(), "fingerprint")


@unittest.skipUnless(
    os.environ.get("WARP_RUN_REAL_RUNTIME_FIXTURE") == "1" and PWSH,
    "opt in to the dependency-free real nextest fixture",
)
class RealNextestTests(Fixture, unittest.TestCase):
    def test_real_prebuilt_reuse_keeps_absolute_out_dir_and_profile_root_assets(self):
        for name in ("cargo", "rustc"):
            (self.tools / name).unlink()
        actual_cargo = shutil.which("cargo")
        self.make_tool(
            "cargo",
            "import pathlib, subprocess, sys\n"
            f"result = subprocess.run([{actual_cargo!r}, *sys.argv[1:]])\n"
            # Cargo emits a macOS-only dSYM alias, even when its destination doesn't exist.
            "for path in pathlib.Path('target/debug/build').glob('*/build-script-build.dSYM'):\n"
            "    if path.is_symlink(): path.unlink()\n"
            "sys.exit(result.returncode)\n",
        )
        self.env.update(RUSTUP_TOOLCHAIN="1.92.0", CARGO_BUILD_JOBS="2")
        (self.repo / "Cargo.toml").write_text(
            '[package]\nname="warp"\nversion="0.1.0"\nedition="2024"\n'
            '[lib]\npath="app/src/server/cloud_objects/update_manager_tests.rs"\n'
            '[workspace]\nmembers=["integration"]\n'
        )
        (self.repo / "integration/src").mkdir(parents=True)
        (self.repo / "integration/Cargo.toml").write_text(
            '[package]\nname="integration"\nversion="0.1.0"\nedition="2024"\n'
        )
        (self.repo / "integration/src/lib.rs").write_text("#[test] fn integration_case() {}\n")
        (self.repo / ".config").mkdir()
        (self.repo / ".config/nextest.toml").write_text('[profile.ci.junit]\npath="junit.xml"\n')
        (self.repo / "build.rs").write_text(
            'fn main() { let out=std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());'
            'std::fs::write(out.join("asset"),"asset").unwrap();'
            'std::fs::write(out.ancestors().nth(3).unwrap().join("fixture-root.dll"),"asset").unwrap(); }'
        )
        functions = "\n".join(
            f"#[test] fn case_{index}() {{ assert_eq!(2 + 2, 4); }}" for index in range(1, 73)
        )
        baseline_source = (
            "mod server { mod cloud_objects { mod update_manager { mod tests {\n"
            "#[test] fn case_0() {"
            'assert_eq!(std::fs::read(concat!(env!("OUT_DIR"),"/asset")).unwrap(),b"asset");'
            "let exe=std::env::current_exe().unwrap();"
            'assert_eq!(std::fs::read(exe.parent().unwrap().parent().unwrap().join("fixture-root.dll"))'
            '.unwrap(),b"asset"); }\n' + functions + "\n}}}}\n"
        )
        self.source.write_text(baseline_source)
        subprocess.run(
            ["cargo", "generate-lockfile"],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            check=True,
        )
        self.commit("real baseline fixture")
        self.baseline = self.git("rev-parse", "HEAD").stdout.strip()
        candidate_source = baseline_source.replace(
            "\n}}}}",
            "\n#[test] fn operation_failure_subscription_retains_completion_before_await() {}\n}}}}",
        )
        self.source.write_text(candidate_source)
        self.commit("real candidate fixture")
        result = self.invoke("Prepare")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.source.write_text("this is intentionally not compilable Rust\n")
        binary_metadata = runtime.read_json(self.private / "candidate/binaries.json")
        binary_path = Path(binary_metadata["rust-binaries"]["warp"]["binary-path"])
        binary_hash = runtime.file_digest(binary_path)
        direct = subprocess.run(
            [
                "cargo",
                "nextest",
                "--no-pager",
                "run",
                "--cargo-metadata",
                str(self.private / "candidate/cargo.json"),
                "--binaries-metadata",
                str(self.private / "candidate/binaries.json"),
                "-E",
                runtime.FILTERS["unit"],
            ],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(direct.returncode, 0, direct.stderr)
        self.assertEqual(runtime.file_digest(binary_path), binary_hash)
        self.git("restore", "--worktree", "--", runtime.SOURCE)
        result = self.invoke("Measure")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = runtime.read_json(self.public / "runtime-comparison.json")
        self.assertTrue(report["complete"])
        self.assertEqual(len(report["samples"]), 8)
        self.assertEqual(self.source.read_text(), candidate_source)
        self.assertEqual((self.repo / "target/debug/fixture-root.dll").read_text(), "asset")
        before = binary_path.stat().st_mtime_ns
        fresh = subprocess.run(
            [
                "cargo",
                "nextest",
                "--no-pager",
                "list",
                "--workspace",
                "--locked",
                "--list-type",
                "binaries-only",
                "--message-format",
                "json",
            ],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(fresh.returncode, 0, fresh.stderr)
        self.assertNotIn("Compiling ", fresh.stderr)
        self.assertEqual(binary_path.stat().st_mtime_ns, before)


if __name__ == "__main__":
    unittest.main()
