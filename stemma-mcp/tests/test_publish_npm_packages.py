#!/usr/bin/env python3
"""Hermetic tests for immutable, ordered npm package publication."""

import base64
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "stemma-mcp" / "npm" / "publish-npm-packages.sh"
VERSION = "1.2.3"
PACKAGES = (
    ("mcp-linux-x64", "@stemma-sh/mcp-linux-x64"),
    ("mcp-linux-arm64", "@stemma-sh/mcp-linux-arm64"),
    ("mcp-darwin-x64", "@stemma-sh/mcp-darwin-x64"),
    ("mcp-darwin-arm64", "@stemma-sh/mcp-darwin-arm64"),
    ("mcp-win32-x64", "@stemma-sh/mcp-win32-x64"),
    ("mcp", "@stemma-sh/mcp"),
)


def sri(seed):
    digest = hashlib.sha512(seed.encode("utf-8")).digest()
    return "sha512-" + base64.b64encode(digest).decode("ascii")


FAKE_NPM = r'''#!/usr/bin/env python3
import base64
import hashlib
import json
import os
import pathlib
import sys


state_path = pathlib.Path(os.environ["FAKE_NPM_STATE"])
log_path = pathlib.Path(os.environ["FAKE_NPM_LOG"])
args = sys.argv[1:]


def load_state():
    return json.loads(state_path.read_text(encoding="utf-8"))


def save_state(state):
    temporary = state_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, sort_keys=True), encoding="utf-8")
    temporary.replace(state_path)


def record():
    with log_path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(args) + "\n")


def package_spec(directory):
    package = json.loads(
        (pathlib.Path(directory) / "package.json").read_text(encoding="utf-8")
    )
    return package["name"], package["version"], package["name"] + "@" + package["version"]


def tarball_filename(name, version):
    return name.lstrip("@").replace("/", "-") + "-" + version + ".tgz"


def integrity(content):
    return "sha512-" + base64.b64encode(hashlib.sha512(content).digest()).decode("ascii")


def missing():
    print(json.dumps({"error": {"code": "E404", "summary": "not found", "detail": ""}}))
    raise SystemExit(1)


record()
if not args:
    raise SystemExit("fake npm received no command")

state = load_state()
command = args[0]

if command == "pack":
    dry_run = args[1:4] == ["--dry-run", "--json", "--ignore-scripts"]
    real_pack = (
        len(args) == 6
        and args[1:4] == ["--json", "--ignore-scripts", "--pack-destination"]
    )
    if not ((len(args) == 5 and dry_run) or real_pack):
        raise SystemExit("unexpected pack arguments: {!r}".format(args))
    package_dir = args[4] if dry_run else args[5]
    name, version, spec = package_spec(package_dir)
    mode = state.get("pack_modes", {}).get(spec)
    if mode == "fail":
        print("synthetic pack failure", file=sys.stderr)
        raise SystemExit(7)
    if mode == "malformed":
        print("{not-json")
        raise SystemExit(0)
    filename = tarball_filename(name, version)
    reported_integrity = state["local_integrities"][spec]
    if real_pack:
        content = spec.encode("utf-8")
        if mode == "actual_drift":
            content += b"-drift"
        tarball = pathlib.Path(args[4]) / filename
        tarball.write_bytes(content)
        reported_integrity = integrity(content)
    print(
        json.dumps(
            [
                {
                    "filename": filename,
                    "integrity": reported_integrity,
                    "name": name,
                    "version": version,
                }
            ]
        )
    )
    raise SystemExit(0)

if command == "view":
    if len(args) != 4 or args[2:] != ["dist.integrity", "--json"]:
        raise SystemExit("unexpected view arguments: {!r}".format(args))
    spec = args[1]
    mode = state.get("view_modes", {}).get(spec)
    if mode == "malformed":
        print("{not-json")
        raise SystemExit(0)
    if mode == "nonstring":
        print(json.dumps({"integrity": state["local_integrities"][spec]}))
        raise SystemExit(0)
    if mode == "error":
        print(json.dumps({"error": {"code": "E500", "summary": "registry failed"}}))
        raise SystemExit(1)

    pending = state.setdefault("pending", {})
    if spec in pending:
        if pending[spec]["remaining"] > 0:
            pending[spec]["remaining"] -= 1
            save_state(state)
            missing()
        state.setdefault("remote", {})[spec] = pending.pop(spec)["integrity"]
        save_state(state)

    if spec not in state.get("remote", {}):
        missing()
    print(json.dumps(state["remote"][spec]))
    raise SystemExit(0)

if command == "publish":
    if len(args) != 6 or args[2:] != ["--access", "public", "--provenance", "--ignore-scripts"]:
        raise SystemExit("unexpected publish arguments: {!r}".format(args))
    spec = pathlib.Path(args[1]).read_text(encoding="utf-8")
    if spec in state.get("publish_fail", []):
        print("synthetic publish failure", file=sys.stderr)
        raise SystemExit(8)
    if spec in state.get("remote", {}) or spec in state.get("pending", {}):
        print("EPUBLISHCONFLICT cannot publish over " + spec, file=sys.stderr)
        raise SystemExit(1)
    # served_integrities models a registry that serves other bytes than the
    # ones uploaded; normally it serves exactly what was published.
    integrity = state.get("served_integrities", {}).get(
        spec, state["local_integrities"][spec]
    )
    delay = state.get("publish_delays", {}).get(spec, 0)
    if delay:
        state.setdefault("pending", {})[spec] = {
            "integrity": integrity,
            "remaining": delay,
        }
    else:
        state.setdefault("remote", {})[spec] = integrity
    save_state(state)
    print("published " + spec)
    raise SystemExit(0)

raise SystemExit("unexpected fake npm command: {!r}".format(args))
'''


class PublishNpmPackagesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.assembled = self.root / "assembled"
        self.fake_bin = self.root / "bin"
        self.home = self.root / "home"
        self.state_path = self.root / "state.json"
        self.log_path = self.root / "npm.jsonl"
        self.assembled.mkdir()
        self.fake_bin.mkdir()
        self.home.mkdir()

        platform_names = [name for _directory, name in PACKAGES[:-1]]
        for directory, name in PACKAGES:
            package_dir = self.assembled / directory
            package_dir.mkdir()
            package = {"name": name, "version": VERSION}
            if directory == "mcp":
                package["optionalDependencies"] = {
                    platform_name: VERSION for platform_name in platform_names
                }
            (package_dir / "package.json").write_text(
                json.dumps(package, indent=2) + "\n", encoding="utf-8"
            )

        fake_npm = self.fake_bin / "npm"
        fake_npm.write_text(textwrap.dedent(FAKE_NPM), encoding="utf-8")
        fake_npm.chmod(0o755)
        self.local_integrities = {
            self.spec(name): sri(name + "@" + VERSION) for _directory, name in PACKAGES
        }
        self.write_state()

    def tearDown(self):
        self.temporary.cleanup()

    @staticmethod
    def spec(name):
        return name + "@" + VERSION

    @property
    def ordered_specs(self):
        return [self.spec(name) for _directory, name in PACKAGES]

    def read_state(self):
        return json.loads(self.state_path.read_text(encoding="utf-8"))

    def write_state(self, **overrides):
        state = {
            "local_integrities": self.local_integrities,
            "pack_modes": {},
            "pending": {},
            "publish_delays": {},
            "publish_fail": [],
            "remote": {},
            "view_modes": {},
        }
        state.update(overrides)
        self.state_path.write_text(json.dumps(state), encoding="utf-8")
        self.log_path.unlink(missing_ok=True)

    def run_script(self, poll_attempts="4", poll_interval="0"):
        environment = os.environ.copy()
        environment["PATH"] = str(self.fake_bin) + os.pathsep + environment["PATH"]
        environment["HOME"] = str(self.home)
        environment["FAKE_NPM_STATE"] = str(self.state_path)
        environment["FAKE_NPM_LOG"] = str(self.log_path)
        for key, value in (
            ("STEMMA_NPM_PUBLISH_POLL_ATTEMPTS", poll_attempts),
            ("STEMMA_NPM_PUBLISH_POLL_INTERVAL_SECONDS", poll_interval),
        ):
            if value is None:
                environment.pop(key, None)
            else:
                environment[key] = value
        environment.pop("NODE_AUTH_TOKEN", None)
        environment.pop("NPM_TOKEN", None)
        return subprocess.run(
            ["bash", str(SCRIPT), str(self.assembled)],
            cwd=str(REPO_ROOT),
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def events(self):
        if not self.log_path.exists():
            return []
        return [
            json.loads(line)
            for line in self.log_path.read_text(encoding="utf-8").splitlines()
        ]

    def published_specs(self):
        filename_to_spec = {
            name.lstrip("@").replace("/", "-") + "-" + VERSION + ".tgz": self.spec(name)
            for _directory, name in PACKAGES
        }
        return [
            filename_to_spec[pathlib.Path(event[1]).name]
            for event in self.events()
            if event[0] == "publish"
        ]

    def assert_no_publish(self):
        self.assertFalse(
            any(event[0] == "publish" for event in self.events()), self.events()
        )

    def set_package_versions(self, version):
        platform_names = [name for _directory, name in PACKAGES[:-1]]
        for directory, name in PACKAGES:
            manifest = self.assembled / directory / "package.json"
            package = json.loads(manifest.read_text(encoding="utf-8"))
            package["version"] = version
            if directory == "mcp":
                package["optionalDependencies"] = {
                    platform_name: version for platform_name in platform_names
                }
            manifest.write_text(json.dumps(package), encoding="utf-8")

    def test_fresh_publish_is_platforms_then_one_wait_then_wrapper(self):
        first_spec = self.ordered_specs[0]
        self.write_state(publish_delays={first_spec: 1})

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.published_specs(), self.ordered_specs)
        events = self.events()
        publish_indices = [
            index for index, event in enumerate(events) if event[0] == "publish"
        ]
        first_publish = publish_indices[0]
        self.assertEqual(
            [event[1] for event in events[:first_publish] if event[0] == "view"],
            self.ordered_specs,
            "every remote version must be reconciled before the first publish",
        )
        platform_publishes = publish_indices[:5]
        wrapper_publish = publish_indices[5]
        self.assertEqual(
            platform_publishes,
            list(range(first_publish, first_publish + 5)),
            "the five platform packages are published back to back",
        )
        between = events[platform_publishes[-1] + 1 : wrapper_publish]
        self.assertTrue(all(event[0] == "view" for event in between), between)
        self.assertEqual(
            {event[1] for event in between},
            set(self.ordered_specs[:5]),
            "every platform package is verified before the wrapper is published",
        )
        self.assertEqual(
            [event[1] for event in between if event[1] == first_spec],
            [first_spec, first_spec],
            "a still-invisible platform is polled again in the same wait",
        )
        self.assertEqual(
            [event[1] for event in events[wrapper_publish + 1 :]],
            [self.ordered_specs[-1]],
            "the wrapper is verified after its own publish",
        )
        for index in publish_indices:
            self.assertEqual(
                events[index][2:],
                ["--access", "public", "--provenance", "--ignore-scripts"],
            )
        self.assertEqual(result.stdout.count("wait ("), 2, result.stdout)

    def test_slow_registry_is_absorbed_by_two_waits(self):
        # npm took 3 to 12 minutes per new package on 2026-11-07. Model that
        # as versions that stay invisible for several polls after publish.
        delays = dict(zip(self.ordered_specs, (3, 1, 5, 0, 2, 4)))
        self.write_state(publish_delays=delays)

        result = self.run_script(poll_attempts="6", poll_interval="0.01")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.published_specs(), self.ordered_specs)
        self.assertEqual(result.stdout.count("wait ("), 2, result.stdout)
        self.assertEqual(result.stdout.count("verified:"), len(PACKAGES))
        events = self.events()
        wrapper_publish = [
            index for index, event in enumerate(events) if event[0] == "publish"
        ][-1]
        slowest = self.ordered_specs[2]
        self.assertEqual(
            sum(1 for event in events[:wrapper_publish] if event[1] == slowest),
            1 + 6,
            "one preflight view, then polls until visible on the sixth attempt",
        )

    def test_visibility_timeout_fails_loudly_and_rerun_resumes(self):
        slow_platform = self.ordered_specs[2]
        wrapper_spec = self.ordered_specs[-1]
        self.write_state(publish_delays={slow_platform: 10})

        timed_out = self.run_script(poll_attempts="3", poll_interval="0")

        self.assertNotEqual(timed_out.returncode, 0)
        self.assertEqual(self.published_specs(), self.ordered_specs[:5])
        self.assertIn(slow_platform, timed_out.stderr)
        self.assertIn("after 3 attempts", timed_out.stderr)
        self.assertIn("Rerun", timed_out.stderr)
        self.assertNotIn("publication complete", timed_out.stdout)

        # Rerunning while npm still hides the version cannot succeed quietly:
        # the version looks missing, and npm refuses to publish it again.
        state = self.read_state()
        self.write_state(**state)
        too_early = self.run_script(poll_attempts="3", poll_interval="0")
        self.assertNotEqual(too_early.returncode, 0)
        self.assertIn("npm publish failed for " + slow_platform, too_early.stderr)
        self.assertNotIn(wrapper_spec, self.published_specs())

        # Once npm serves it, a rerun verifies every platform integrity,
        # skips them, and finishes with the wrapper.
        state = self.read_state()
        state["remote"][slow_platform] = state["pending"].pop(slow_platform)["integrity"]
        self.write_state(**state)
        resumed = self.run_script(poll_attempts="3", poll_interval="0")

        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertEqual(self.published_specs(), [wrapper_spec])
        self.assertEqual(resumed.stdout.count("skip:"), 5)
        self.assertIn("publication complete", resumed.stdout)

    def test_visible_version_with_wrong_integrity_fails_during_wait(self):
        target = self.ordered_specs[1]
        # The registry serves different bytes than the ones just published.
        self.write_state(served_integrities={target: sri("tampered")})

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(target, result.stderr)
        self.assertIn(sri("tampered"), result.stderr)
        self.assertNotIn(self.ordered_specs[-1], self.published_specs())

    def test_default_window_is_about_fifteen_minutes(self):
        self.write_state()

        result = self.run_script(poll_attempts=None, poll_interval=None)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("up to 60 attempts x 15s", result.stdout)

    def test_release_workflow_sets_the_window_explicitly(self):
        workflow = (REPO_ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        step = workflow.index("run: bash stemma-mcp/npm/publish-npm-packages.sh out")
        step_start = workflow.rindex("      - name:", 0, step)
        step_text = workflow[step_start:step]
        self.assertIn('STEMMA_NPM_PUBLISH_POLL_ATTEMPTS: "60"', step_text)
        self.assertIn('STEMMA_NPM_PUBLISH_POLL_INTERVAL_SECONDS: "15"', step_text)

    def test_identical_retry_skips_every_publish(self):
        self.write_state(remote=dict(self.local_integrities))

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_no_publish()
        self.assertEqual(result.stdout.count("skip:"), len(PACKAGES))

    def test_late_integrity_mismatch_refuses_before_any_publish(self):
        wrapper_spec = self.ordered_specs[-1]
        self.write_state(remote={wrapper_spec: sri("different-wrapper")})

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assert_no_publish()
        self.assertIn(wrapper_spec, result.stderr)
        self.assertIn(self.local_integrities[wrapper_spec], result.stderr)
        self.assertIn(sri("different-wrapper"), result.stderr)

    def test_malformed_pack_json_and_local_pack_failure_fail_closed(self):
        wrapper_spec = self.ordered_specs[-1]
        for mode in ("malformed", "fail", "actual_drift"):
            with self.subTest(mode=mode):
                self.write_state(pack_modes={wrapper_spec: mode})

                result = self.run_script()

                self.assertNotEqual(result.returncode, 0)
                self.assert_no_publish()
                self.assertIn(wrapper_spec, result.stderr)

    def test_malformed_local_manifest_fails_before_npm(self):
        manifest = self.assembled / PACKAGES[0][0] / "package.json"
        manifest.write_text("{not-json", encoding="utf-8")

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assert_no_publish()
        self.assertIn("cannot parse", result.stderr)

    def test_lifecycle_scripts_are_refused_before_npm(self):
        manifest = self.assembled / PACKAGES[0][0] / "package.json"
        package = json.loads(manifest.read_text(encoding="utf-8"))
        package["scripts"] = {"prepack": "change-the-package"}
        manifest.write_text(json.dumps(package), encoding="utf-8")

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assert_no_publish()
        self.assertFalse(self.events())
        self.assertIn("lifecycle scripts", result.stderr)

    def test_prerelease_and_build_metadata_are_refused_before_npm(self):
        for version in ("1.2.3-rc.1", "1.2.3+qualified.1"):
            with self.subTest(version=version):
                self.set_package_versions(version)
                self.write_state()

                result = self.run_script()

                self.assertNotEqual(result.returncode, 0)
                self.assert_no_publish()
                self.assertFalse(self.events())
                self.assertIn("stable MAJOR.MINOR.PATCH", result.stderr)
                self.set_package_versions(VERSION)

    def test_malformed_or_failed_registry_json_fails_before_publish(self):
        wrapper_spec = self.ordered_specs[-1]
        for mode in ("malformed", "nonstring", "error"):
            with self.subTest(mode=mode):
                self.write_state(view_modes={wrapper_spec: mode})

                result = self.run_script()

                self.assertNotEqual(result.returncode, 0)
                self.assert_no_publish()
                self.assertIn(wrapper_spec, result.stderr)

    def test_platform_publish_failure_never_reaches_wrapper(self):
        failing_spec = self.ordered_specs[2]
        self.write_state(publish_fail=[failing_spec])

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.published_specs(), self.ordered_specs[:3])
        self.assertNotIn(self.ordered_specs[-1], self.published_specs())


if __name__ == "__main__":
    unittest.main()
