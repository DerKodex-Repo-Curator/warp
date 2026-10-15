"""Private prebuilt nextest orchestration; only an allowlisted summary is published."""

import argparse
from contextlib import contextmanager
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import time
import xml.etree.ElementTree as ET


BASELINE = "210ffd656b98e7d3493fb99ac5024b88ff213b28"
SOURCE = "app/src/server/cloud_objects/update_manager_tests.rs"
FAMILY_PREFIX = "server::cloud_objects::update_manager::tests::"
ADDED_TEST = FAMILY_PREFIX + "operation_failure_subscription_retains_completion_before_await"
FILTERS = {
    "family": "test(server::cloud_objects::update_manager::tests::)",
    "unit": "not package(integration)",
}
ORDER = ("candidate", "baseline", "baseline", "candidate")
TOOL_FILES = {
    ".github/workflows/ci.yml",
    "script/windows/compare_cloud_operation_runtime.ps1",
    "script/windows/runtime_comparison.py",
    "script/windows/compare_cloud_operation_runtime_tests.py",
    "script/windows/measure_ci_performance.ps1",
}
NORMAL_FLAGS = "-C debuginfo=line-tables-only --cfg=web_sys_unstable_apis"


class ComparisonError(Exception):
    """Codes, never native stderr or input strings, may cross the publication boundary."""


def require(condition, code):
    if not condition:
        raise ComparisonError(code)


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def file_digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def plain_path(path):
    """Reject symlinks and Windows junctions before copying or deleting owned trees."""
    for item in (path, *path.parents):
        if item.exists() or item.is_symlink():
            info = item.lstat()
            require(
                not stat.S_ISLNK(info.st_mode)
                and not getattr(info, "st_file_attributes", 0) & 0x400,
                "redirected-path",
            )


def tree_inventory(root):
    plain_path(root)
    require(root.is_dir(), "missing-target-tree")
    result = {}
    for parent, directories, files in os.walk(root, followlinks=False):
        for name in directories + files:
            path = Path(parent) / name
            info = path.lstat()
            require(
                not stat.S_ISLNK(info.st_mode)
                and not getattr(info, "st_file_attributes", 0) & 0x400,
                "redirected-target-entry",
            )
            if name in files:
                require(stat.S_ISREG(info.st_mode), "nonregular-target-entry")
                result[path.relative_to(root).as_posix()] = info.st_size
    require(result, "empty-target-tree")
    return result


def snapshot_target(target, snapshot):
    require(not snapshot.exists(), "existing-snapshot")
    plain_path(snapshot)
    inventory = tree_inventory(target)
    # Reserve another complete copy and headroom before consuming the snapshot volume.
    require(
        shutil.disk_usage(snapshot.parent).free >= 2 * sum(inventory.values()) + 1024**3,
        "insufficient-snapshot-space",
    )
    shutil.copytree(target, snapshot, copy_function=shutil.copy2)
    require(tree_inventory(snapshot) == inventory, "incomplete-snapshot")
    return digest(inventory)


def restore_target(snapshot, target, expected_inventory):
    inventory = tree_inventory(snapshot)
    require(digest(inventory) == expected_inventory, "changed-snapshot")
    plain_path(target)
    if target.exists():
        tree_inventory(target)
    require(
        shutil.disk_usage(target.parent).free
        + (sum(tree_inventory(target).values()) if target.exists() else 0)
        >= sum(inventory.values()) + 1024**3,
        "insufficient-restore-space",
    )
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(snapshot, target, copy_function=shutil.copy2)
    require(tree_inventory(target) == inventory, "incomplete-target-restore")


class Roster(dict):
    def __init__(self):
        super().__init__()
        self.suites = {}


def parse_roster(document):
    suites = document.get("rust-suites")
    require(isinstance(suites, dict) and suites, "invalid-list-schema")
    roster = Roster()
    for binary_id, suite in suites.items():
        require(suite.get("status") in ("listed", "skipped"), "invalid-suite-status")
        roster.suites[binary_id] = suite["status"]
        require(suite["status"] == "listed" or not suite["testcases"], "invalid-skipped-suite")
        for name, case in suite["testcases"].items():
            require(isinstance(case.get("ignored"), bool), "invalid-list-schema")
            require(case.get("kind") == "test", "invalid-list-schema")
            status = case.get("filter-match", {})
            require(status.get("status") in ("matches", "mismatch"), "invalid-list-schema")
            roster[(binary_id, name)] = (case["ignored"], json.dumps(status, sort_keys=True))
    require(len(roster) == document.get("test-count"), "incomplete-list")
    return roster


def matched_tests(roster):
    return {
        key
        for key, (ignored, status) in roster.items()
        if not ignored and json.loads(status)["status"] == "matches"
    }


def roster_hash(roster):
    return digest({"suites": roster.suites, "tests": sorted(roster.items())})


def compare_rosters(baseline, candidate):
    require(baseline.suites == candidate.suites, "changed-binary-filter-status")
    require(
        all(candidate.get(key) == value for key, value in baseline.items()), "changed-test-roster"
    )
    require(set(candidate) - set(baseline) == {("warp", ADDED_TEST)}, "unexpected-added-tests")


def finite_number(value):
    result = float(value)
    require(math.isfinite(result) and result >= 0, "invalid-junit-number")
    return result


def parse_junit(path, roster):
    require(path.is_file(), "missing-junit")
    text = path.read_text(encoding="utf-8-sig")
    require("<!DOCTYPE" not in text and "<!ENTITY" not in text, "invalid-junit")
    root = ET.fromstring(text)
    require(root.tag == "testsuites", "invalid-junit-schema")
    totals = {}
    for name in ("tests", "skipped", "failures", "errors"):
        require(re.fullmatch(r"[0-9]+", root.attrib.get(name, "")), "invalid-junit-count")
        totals[name] = int(root.attrib[name])
    cases = root.findall("./testsuite/testcase")
    for name, tag in (("skipped", "skipped"), ("failures", "failure"), ("errors", "error")):
        require(
            sum(case.find(tag) is not None for case in cases) == totals[name],
            "inconsistent-junit-counts",
        )
    observed = {(case.attrib["classname"], case.attrib["name"]) for case in cases}
    require(len(observed) == len(cases) == totals["tests"], "duplicate-or-missing-junit-cases")
    require(observed == matched_tests(roster), "junit-roster-mismatch")
    elapsed = [(case, finite_number(case.attrib["time"])) for case in cases]
    retries = [
        node
        for case in cases
        for node in case
        if node.tag in ("rerunFailure", "rerunError", "flakyFailure", "flakyError")
    ]
    family = sorted(
        (
            {
                "test_id_sha256": digest((case.attrib["classname"], case.attrib["name"])),
                "elapsed_seconds": seconds,
            }
            for case, seconds in elapsed
            if case.attrib["classname"] == "warp" and case.attrib["name"].startswith(FAMILY_PREFIX)
        ),
        key=lambda item: item["test_id_sha256"],
    )
    require(len(family) <= 74, "unexpected-family-size")
    return {
        **totals,
        "nextest_wall_seconds": finite_number(root.attrib["time"]),
        "summed_test_elapsed_seconds": sum(seconds for _, seconds in elapsed),
        # Absent retry elements don't establish zero retries for every JUnit producer.
        "recorded_retry_failures": len(retries) if retries else None,
        "leaky_passes": None,
        "roster_sha256": roster_hash(roster),
        "family_test_elapsed": family,
    }


class RuntimeComparison:
    def __init__(self, root, mode, baseline):
        self.root = root
        self.mode = mode
        self.baseline = baseline
        self.candidate = None
        self.private = None
        self.public = None
        self.target = root / "target"
        self.state = None
        self.can_save = False
        self.restore_target_on_exit = False
        self.summary = {
            "schema": 1,
            "complete": False,
            "issue": None,
            "samples": [],
            "python_version": list(sys.version_info[:3]),
        }

    def native(self, args, output=None, code="native-command-failed"):
        result = subprocess.run(args, cwd=self.root, capture_output=True)
        if output is not None:
            output.write_bytes(result.stdout)
            output.with_suffix(output.suffix + ".stderr").write_bytes(result.stderr)
        require(result.returncode == 0, code)
        return result.stdout.decode("utf-8", "strict").strip()

    def git(self, *args):
        return self.native(["git", "--no-pager", *args], code="git-command-failed")

    def guard(self):
        env = os.environ
        require(sys.version_info >= (3, 9), "python-3-9-required")
        require(
            env.get("GITHUB_ACTIONS") == "true" and env.get("RUNNER_OS") == "Windows",
            "not-windows-ci",
        )
        require(
            self.root == Path(env["GITHUB_WORKSPACE"]).resolve() == Path.cwd().resolve(),
            "not-canonical-workspace",
        )
        plain_path(self.root)
        require(
            env.get("CARGO_INCREMENTAL") == "0" and env.get("RUSTFLAGS") == NORMAL_FLAGS,
            "unexpected-build-flags",
        )
        require(
            env.get("NEXTEST_PROFILE") == "ci"
            and env.get("WORKSPACE_TEST_ARGS", "").split() == ["--workspace", "--locked"],
            "unexpected-test-configuration",
        )
        blocked = {
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET",
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_BUILD_INCREMENTAL",
            "CARGO_BUILD_BUILD_DIR",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTC_BOOTSTRAP",
            "RUSTC",
            "CARGO_BUILD_RUSTC",
            "CARGO_ENCODED_RUSTFLAGS",
            "NEXTEST_TEST_THREADS",
        }
        require(
            not any(
                value
                and (
                    name in blocked
                    or name.startswith("CARGO_PROFILE_")
                    or (name.startswith("CARGO_TARGET_") and name.endswith("RUSTFLAGS"))
                )
                for name, value in env.items()
            ),
            "unexpected-build-override",
        )
        if self.mode == "Measure":
            require(not env.get("GITHUB_TOKEN"), "build-auth-in-measurement")
        require(re.fullmatch(r"[0-9a-f]{40}", self.baseline), "invalid-baseline")
        candidate = self.git("rev-parse", "HEAD")
        require(re.fullmatch(r"[0-9a-f]{40}", candidate), "invalid-candidate")
        if subprocess.run(
            ["git", "cat-file", "-e", self.baseline + "^{commit}"],
            cwd=self.root,
            capture_output=True,
        ).returncode:
            require(self.mode == "Prepare", "missing-baseline")
            self.git("fetch", "--no-tags", "--depth=1", "origin", self.baseline)
        changes = set(self.git("diff", "--name-only", self.baseline, candidate).splitlines())
        require(
            SOURCE in changes and changes <= TOOL_FILES | {SOURCE},
            "unexpected-source-scope",
        )
        require(not self.git("diff", "--name-only", "HEAD", "--", SOURCE), "dirty-test-source")
        require(not self.git("diff", "--cached", "--name-only"), "dirty-index")
        require(
            not self.git(
                "diff",
                "--name-only",
                "HEAD",
                "--",
                "app",
                "crates",
                ".cargo",
                ".config",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
                "build.rs",
            ),
            "dirty-build-inputs",
        )
        require(
            not self.git(
                "ls-files",
                "--others",
                "--exclude-standard",
                "--",
                "app",
                "crates",
                ".cargo",
                ".config",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
                "build.rs",
            ),
            "untracked-build-inputs",
        )
        require(self.native(["rustc", "--version"]).startswith("rustc 1.92.0 "), "wrong-rustc")
        require(self.native(["cargo", "--version"]).startswith("cargo 1.92.0 "), "wrong-cargo")
        require(
            self.native(["cargo", "nextest", "--version"])
            .splitlines()[0]
            .startswith("cargo-nextest 0.9.143 "),
            "wrong-nextest",
        )
        tag = f'{env["GITHUB_RUN_ID"]}-{env["GITHUB_RUN_ATTEMPT"]}'
        require(re.fullmatch(r"[0-9]+-[0-9]+", tag), "invalid-run-identity")
        runner = Path(env["RUNNER_TEMP"]).resolve()
        require(runner.is_dir(), "missing-runner-temp")
        self.private = runner / f"warp-cloud-operation-runtime-{tag}"
        self.public = runner / f"warp-windows-ci-{tag}"
        require(
            not runner.is_relative_to(self.root) and not self.root.is_relative_to(runner),
            "overlapping-private-workspace",
        )
        for path in (self.private, self.public, self.target):
            plain_path(path)
        self.candidate = candidate
        self.summary.update(baseline=self.baseline, candidate=self.candidate)

    def restore_source(self, variant):
        source = self.candidate if variant == "candidate" else self.baseline
        result = subprocess.run(
            ["git", "--no-pager", "diff", "--quiet", source, "--", SOURCE],
            cwd=self.root,
            capture_output=True,
        )
        require(result.returncode in (0, 1), "source-comparison-failed")
        if result.returncode:
            self.git("restore", "--source", source, "--worktree", "--", SOURCE)
        self.git("diff", "--exit-code", source, "--", SOURCE)

    def prebuilt(self, variant, command, filter_name):
        args = [
            "cargo",
            "nextest",
            "--no-pager",
            command,
            "--cargo-metadata",
            str(self.private / variant / "cargo.json"),
            "--binaries-metadata",
            str(self.private / variant / "binaries.json"),
        ]
        args += ["-E", FILTERS[filter_name]]
        if command == "list":
            args += ["--message-format", "json"]
        return args

    def binary_provenance(self, metadata):
        build = metadata["rust-build-meta"]
        require(
            Path(build["target-directory"]).resolve() == self.target
            and Path(build.get("build-directory", build["target-directory"])).resolve()
            == self.target,
            "relocated-build-metadata",
        )
        binaries = list(metadata["rust-binaries"].values())
        selected = [
            item for item in binaries if item["binary-id"] == "warp" and item["kind"] == "lib"
        ]
        require(len(selected) == 1, "missing-warp-libtest")
        for binary in binaries:
            path = Path(binary["binary-path"])
            plain_path(path)
            require(path.is_relative_to(self.target) and path.is_file(), "invalid-binary-path")
        return file_digest(Path(selected[0]["binary-path"]))

    @contextmanager
    def resources(self, label, phase):
        output = self.public / label
        private = self.private / (label + "-collector.log")
        collector = self.root / "script/windows/measure_ci_performance.ps1"
        command = [
            "pwsh",
            "-NoProfile",
            "-File",
            str(collector),
            "-OutputDirectory",
            str(output),
        ]
        record = {"collector_errors": 0, "collector_complete": False}
        process = None
        with private.open("wb") as log:
            try:
                self.native(command + ["-Mode", "Start"], private)
                require((output / "metadata.json").is_file(), "collector-start-failed")
                if phase == "unit-tests":
                    self.native(command + ["-Mode", "Phase", "-Phase", phase], private)
                process = subprocess.Popen(
                    command + ["-Mode", "Collect"],
                    cwd=self.root,
                    stdout=log,
                    stderr=log,
                )
                deadline = time.monotonic() + 30
                while not (output / "entered").is_file():
                    require(process.poll() is None, "collector-exited-before-sample")
                    require(time.monotonic() < deadline, "collector-start-timeout")
                    time.sleep(0.05)
                yield record
            finally:
                if process is not None:
                    try:
                        self.native(command + ["-Mode", "Stop"], private)
                        process.wait(timeout=15)
                    except (ComparisonError, subprocess.TimeoutExpired):
                        record["collector_errors"] += 1
                        if process.poll() is None:
                            process.kill()
                            process.wait()
                    record["collector_complete"] = (
                        process.returncode == 0 and (output / "completed").is_file()
                    )
                errors = output / "errors.jsonl"
                if errors.is_file():
                    record["collector_errors"] += len(errors.read_text().splitlines())

    def save(self):
        if self.can_save and self.public is not None and self.public.is_dir():
            write_json(self.public / "runtime-comparison.json", self.summary)

    def prepare(self):
        require(not self.private.exists() and not self.public.exists(), "existing-comparison-state")
        self.private.mkdir()
        self.public.mkdir()
        self.can_save = True
        self.state = {"baseline": self.baseline, "candidate": self.candidate, "variants": {}}
        graph = None
        for variant in ("baseline", "candidate"):
            self.restore_source(variant)
            source_hash = file_digest(self.root / SOURCE)
            directory = self.private / variant
            directory.mkdir()
            with self.resources("prepare-" + variant, "compile") as resource:
                self.native(
                    ["cargo", "nextest", "run", "--workspace", "--locked", "--no-run", "--timings"],
                    directory / "compile.stdout",
                    "prepare-build-failed",
                )
            require(
                resource["collector_complete"] and resource["collector_errors"] == 0,
                "prepare-collector-incomplete",
            )
            metadata_file = directory / "cargo.json"
            self.native(
                ["cargo", "metadata", "--format-version", "1", "--locked"],
                metadata_file,
                "cargo-metadata-failed",
            )
            metadata = read_json(metadata_file)
            require(
                Path(metadata["workspace_root"]).resolve() == self.root
                and Path(metadata["target_directory"]).resolve() == self.target,
                "relocated-cargo-metadata",
            )
            if graph is None:
                graph = metadata
            require(graph == metadata, "changed-workspace-graph")
            binaries_file = directory / "binaries.json"
            self.native(
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
                binaries_file,
                "binary-metadata-failed",
            )
            binary_hash = self.binary_provenance(read_json(binaries_file))
            require(file_digest(self.root / SOURCE) == source_hash, "source-changed-during-prepare")
            inventory_hash = snapshot_target(self.target, directory / "target")
            self.state["variants"][variant] = {
                "binary_sha256": binary_hash,
                "source_sha256": source_hash,
                "source_mtime_ns": (self.root / SOURCE).stat().st_mtime_ns,
                "target_inventory_sha256": inventory_hash,
                "cargo_metadata_sha256": file_digest(metadata_file),
                "binaries_metadata_sha256": file_digest(binaries_file),
            }
        write_json(self.private / "state.json", self.state)
        self.summary["prepared"] = True

    def restore_variant(self, variant):
        self.restore_source(variant)
        expected = self.state["variants"][variant]
        directory = self.private / variant
        plain_path(self.root / SOURCE)
        require(
            file_digest(self.root / SOURCE) == expected["source_sha256"],
            "source-provenance-mismatch",
        )
        for name in ("cargo", "binaries"):
            require(
                file_digest(directory / (name + ".json")) == expected[name + "_metadata_sha256"],
                "changed-private-metadata",
            )
        restore_target(directory / "target", self.target, expected["target_inventory_sha256"])
        require(
            self.binary_provenance(read_json(directory / "binaries.json"))
            == expected["binary_sha256"],
            "binary-provenance-mismatch",
        )
        require(
            file_digest(self.root / SOURCE) == expected["source_sha256"],
            "source-provenance-mismatch",
        )
        # Cargo's next doc-test freshness check must see the source timestamp used by this build.
        os.utime(self.root / SOURCE, ns=(expected["source_mtime_ns"], expected["source_mtime_ns"]))

    def measure(self):
        require((self.private / "state.json").is_file(), "missing-prepared-state")
        self.state = read_json(self.private / "state.json")
        require(
            self.state["baseline"] == self.baseline and self.state["candidate"] == self.candidate,
            "changed-comparison-head",
        )
        require(self.public.is_dir(), "missing-measurement-directory")
        self.can_save = True
        self.restore_target_on_exit = True
        require(
            not (self.private / "measure-started").exists(),
            "measurement-already-started",
        )
        (self.private / "measure-started").touch()
        rosters = {}
        for variant in ("baseline", "candidate"):
            self.restore_variant(variant)
            rosters[variant] = {}
            for scope in FILTERS:
                path = self.private / variant / (scope + "-list.json")
                self.native(
                    self.prebuilt(variant, "list", scope),
                    path,
                    "prebuilt-inventory-failed",
                )
                rosters[variant][scope] = parse_roster(read_json(path))
        for scope in FILTERS:
            compare_rosters(rosters["baseline"][scope], rosters["candidate"][scope])
        require(len(matched_tests(rosters["baseline"]["family"])) == 73, "wrong-baseline-family")
        require(len(matched_tests(rosters["candidate"]["family"])) == 74, "wrong-candidate-family")
        self.summary["coverage"] = {
            variant: {
                scope: {
                    "roster_sha256": roster_hash(roster),
                    "listed": len(roster),
                    "matched": len(matched_tests(roster)),
                    "ignored": sum(ignored for ignored, _ in roster.values()),
                    "unignored_filtered": sum(
                        not ignored and json.loads(status)["status"] == "mismatch"
                        for ignored, status in roster.values()
                    ),
                    "skipped_binaries": sum(
                        status == "skipped" for status in roster.suites.values()
                    ),
                }
                for scope, roster in scopes.items()
            }
            for variant, scopes in rosters.items()
        }
        self.summary["expected_added_test"] = ADDED_TEST
        for scope in FILTERS:
            for index, variant in enumerate(ORDER, 1):
                label = f"{scope}-{index}-{variant}"
                sample = {
                    "scope": scope,
                    "index": index,
                    "variant": variant,
                    "exit_code": None,
                    "process_elapsed_seconds": None,
                    "complete": False,
                }
                self.summary["samples"].append(sample)
                try:
                    self.restore_variant(variant)
                    junit = self.target / "nextest/ci/junit.xml"
                    plain_path(junit)
                    junit.unlink(missing_ok=True)
                    arguments = self.prebuilt(variant, "run", scope)
                    with self.resources(label, "unit-tests") as resource:
                        with (
                            (self.private / (label + ".stdout")).open("wb") as stdout,
                            (self.private / (label + ".stderr")).open("wb") as stderr,
                        ):
                            # All inventory, copies, collector setup and parsing stay outside this clock.
                            start = time.perf_counter()
                            result = subprocess.run(
                                arguments,
                                cwd=self.root,
                                stdout=stdout,
                                stderr=stderr,
                            )
                            sample["process_elapsed_seconds"] = time.perf_counter() - start
                            sample["exit_code"] = result.returncode
                    sample.update(resource)
                    sample["source_provenance_unchanged"] = (
                        file_digest(self.root / SOURCE)
                        == self.state["variants"][variant]["source_sha256"]
                    )
                    require(sample["source_provenance_unchanged"], "source-changed-during-run")
                    if junit.is_file():
                        shutil.copy2(junit, self.private / (label + ".junit.xml"))
                    binary_hash = self.binary_provenance(
                        read_json(self.private / variant / "binaries.json")
                    )
                    sample["binary_provenance_unchanged"] = (
                        binary_hash == self.state["variants"][variant]["binary_sha256"]
                    )
                    sample["warp_libtest_sha256"] = binary_hash
                    require(sample["binary_provenance_unchanged"], "binary-changed-during-run")
                    sample.update(parse_junit(junit, rosters[variant][scope]))
                    require(result.returncode == 0, "test-process-failed")
                    require(sample["failures"] == sample["errors"] == 0, "test-report-failed")
                    require(
                        resource["collector_complete"] and resource["collector_errors"] == 0,
                        "sample-collector-incomplete",
                    )
                    sample["complete"] = True
                finally:
                    self.save()
        self.summary["complete"] = True

    def run(self):
        self.guard()
        try:
            if self.mode == "Prepare":
                self.prepare()
            else:
                self.measure()
        except ComparisonError as error:
            self.summary["issue"] = str(error)
            raise
        except Exception:
            self.summary["issue"] = "unexpected-private-evidence-error"
            raise ComparisonError("unexpected-private-evidence-error") from None
        finally:
            try:
                self.restore_source("candidate")
                if (
                    self.restore_target_on_exit
                    and self.state is not None
                    and "candidate" in self.state.get("variants", {})
                ):
                    self.restore_variant("candidate")
            except Exception:
                self.summary["complete"] = False
                self.summary["cleanup_failed"] = True
                self.save()
                raise ComparisonError("candidate-cleanup-failed") from None
            self.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("Prepare", "Measure"), required=True)
    parser.add_argument("--baseline", default=BASELINE)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    try:
        RuntimeComparison(root, args.mode, args.baseline).run()
    except ComparisonError as error:
        print(f"Runtime comparison rejected: {error}", file=sys.stderr)
        return 1
    except Exception:
        print("Runtime comparison rejected: invalid-private-evidence", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
