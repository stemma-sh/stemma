#!/usr/bin/env python3
"""Hermetic tests for immutable crates.io publication (scripts/publish-crate.sh).

A fake `cargo` on PATH packages and "publishes" into a local HTTP server that
speaks the crates.io sparse-index protocol, so registry delays, conflicts
and failures are exercised without network or credentials.
"""

import hashlib
import http.server
import json
import os
import pathlib
import re
import subprocess
import tempfile
import textwrap
import threading
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "publish-crate.sh"
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "release.yml"
JUSTFILE = REPO_ROOT / "Justfile"
VERSION = "1.2.3"
CRATE = "stemma-diff"
CRATE_ORDER = ("stemma", "stemma-diff", "stemma-artifacts", "stemma-cli")


def index_path(name):
    lower = name.lower()
    if len(lower) <= 2:
        return "/{}/{}".format(len(lower), lower)
    if len(lower) == 3:
        return "/3/{}/{}".format(lower[0], lower)
    return "/{}/{}/{}".format(lower[0:2], lower[2:4], lower)


def crate_bytes(name, version):
    return "{}@{} package".format(name, version).encode("utf-8")


def cksum(content):
    return hashlib.sha256(content).hexdigest()


FAKE_CARGO = r'''#!/usr/bin/env python3
import json
import os
import pathlib
import sys

state_path = pathlib.Path(os.environ["FAKE_CARGO_STATE"])
log_path = pathlib.Path(os.environ["FAKE_CARGO_LOG"])
args = sys.argv[1:]
with log_path.open("a", encoding="utf-8") as stream:
    stream.write(json.dumps(args) + "\n")
state = json.loads(state_path.read_text(encoding="utf-8"))


def save():
    temporary = state_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state), encoding="utf-8")
    temporary.replace(state_path)


def package_entry(name):
    for entry in state["packages"]:
        if entry["name"] == name:
            return entry
    raise SystemExit("fake cargo: unknown package " + name)


if args == ["metadata", "--locked", "--no-deps", "--format-version", "1"]:
    print(json.dumps({"packages": state["packages"], "target_directory": state["target"]}))
    raise SystemExit(0)

if args[:3] == ["package", "--locked", "-p"] and args[4:] == ["--no-verify"]:
    entry = package_entry(args[3])
    spec = args[3] + "@" + entry["version"]
    package_dir = pathlib.Path(state["target"]) / "package"
    package_dir.mkdir(parents=True, exist_ok=True)
    content = (spec + " package").encode("utf-8")
    (package_dir / "{}-{}.crate".format(args[3], entry["version"])).write_bytes(content)
    raise SystemExit(0)

if args[:3] == ["publish", "--locked", "-p"] and len(args) in (4, 5):
    name = args[3]
    entry = package_entry(name)
    spec = name + "@" + entry["version"]
    if len(args) == 5:
        if args[4] != "--dry-run":
            raise SystemExit("fake cargo: unexpected publish arguments {!r}".format(args))
        if spec in state.get("dry_run_fail", []):
            print("synthetic verification failure", file=sys.stderr)
            raise SystemExit(101)
        raise SystemExit(0)
    if "CARGO_REGISTRY_TOKEN" in os.environ and os.environ["CARGO_REGISTRY_TOKEN"] != "fake-token":
        raise SystemExit("fake cargo: unexpected token")
    if spec in state.get("publish_fail", []):
        print("synthetic publish failure", file=sys.stderr)
        raise SystemExit(101)
    if spec in state["index"] or spec in state["pending"]:
        print("error: crate version `{}` is already uploaded".format(spec), file=sys.stderr)
        raise SystemExit(101)
    content = (pathlib.Path(state["target"]) / "package" / "{}-{}.crate".format(name, entry["version"])).read_bytes()
    import hashlib
    record = {
        "name": name,
        "vers": entry["version"],
        "cksum": state.get("served_cksums", {}).get(spec, hashlib.sha256(content).hexdigest()),
        "yanked": False,
        "deps": [],
        "features": {},
    }
    delay = state.get("publish_delays", {}).get(spec, 0)
    if delay:
        state["pending"][spec] = {"record": record, "remaining": delay}
    else:
        state["index"][spec] = record
    save()
    print("Uploaded " + spec)
    raise SystemExit(0)

raise SystemExit("fake cargo: unexpected command {!r}".format(args))
'''


class IndexHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        test = self.server.test
        with test.lock:
            state = test.read_state()
            test.index_requests.append(self.path)
            mode = state.get("index_mode")
            if mode == "error":
                self.send_response(500)
                self.end_headers()
                return
            lines = []
            for spec, pending in list(state["pending"].items()):
                if index_path(pending["record"]["name"]) != self.path:
                    continue
                if pending["remaining"] > 0:
                    pending["remaining"] -= 1
                else:
                    state["index"][spec] = state["pending"].pop(spec)["record"]
            test.save_state(state)
            for record in state["index"].values():
                if index_path(record["name"]) == self.path:
                    lines.append(json.dumps(record))
            if mode == "malformed":
                lines.append("{not-json")
        if not lines:
            self.send_response(404)
            self.end_headers()
            return
        body = ("\n".join(lines) + "\n").encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class PublishCrateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.checkout = self.root / "checkout"
        self.fake_bin = self.root / "bin"
        self.state_path = self.root / "state.json"
        self.log_path = self.root / "cargo.jsonl"
        self.checkout.mkdir()
        self.fake_bin.mkdir()
        self.lock = threading.Lock()
        self.index_requests = []

        git = ["git", "-C", str(self.checkout)]
        subprocess.run(git + ["init", "-q"], check=True)
        (self.checkout / ".gitignore").write_text("target/\n", encoding="utf-8")
        (self.checkout / "README").write_text("fixture\n", encoding="utf-8")
        subprocess.run(git + ["add", "-A"], check=True)
        subprocess.run(
            git
            + ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "init"],
            check=True,
        )

        fake_cargo = self.fake_bin / "cargo"
        fake_cargo.write_text(textwrap.dedent(FAKE_CARGO), encoding="utf-8")
        fake_cargo.chmod(0o755)
        self.write_state()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), IndexHandler)
        self.server.test = self
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.temporary.cleanup()

    @property
    def spec(self):
        return CRATE + "@" + VERSION

    @property
    def local_cksum(self):
        return cksum(crate_bytes(CRATE, VERSION))

    def read_state(self):
        return json.loads(self.state_path.read_text(encoding="utf-8"))

    def save_state(self, state):
        temporary = self.state_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(state), encoding="utf-8")
        temporary.replace(self.state_path)

    def write_state(self, **overrides):
        state = {
            "packages": [
                {"name": name, "version": VERSION, "publish": None} for name in CRATE_ORDER
            ]
            + [{"name": "stemma-api", "version": VERSION, "publish": []}],
            "target": str(self.checkout / "target"),
            "index": {},
            "pending": {},
        }
        state.update(overrides)
        self.save_state(state)
        self.log_path.unlink(missing_ok=True)
        self.index_requests = []

    def index_record(self, checksum, yanked=False):
        return {
            "name": CRATE,
            "vers": VERSION,
            "cksum": checksum,
            "yanked": yanked,
            "deps": [],
            "features": {},
        }

    def run_script(self, package=CRATE, poll_attempts="4", poll_interval="0"):
        environment = os.environ.copy()
        environment["PATH"] = str(self.fake_bin) + os.pathsep + environment["PATH"]
        environment["FAKE_CARGO_STATE"] = str(self.state_path)
        environment["FAKE_CARGO_LOG"] = str(self.log_path)
        environment["STEMMA_CRATES_INDEX_URL"] = "http://127.0.0.1:{}".format(
            self.server.server_address[1]
        )
        environment["STEMMA_CRATES_PUBLISH_POLL_ATTEMPTS"] = poll_attempts
        environment["STEMMA_CRATES_PUBLISH_POLL_INTERVAL_SECONDS"] = poll_interval
        environment["CARGO_REGISTRY_TOKEN"] = "fake-token"
        return subprocess.run(
            ["bash", str(SCRIPT), package],
            cwd=str(self.checkout),
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def cargo_calls(self):
        if not self.log_path.exists():
            return []
        return [
            json.loads(line)
            for line in self.log_path.read_text(encoding="utf-8").splitlines()
        ]

    def real_publishes(self):
        return [
            call for call in self.cargo_calls() if call[:1] == ["publish"] and "--dry-run" not in call
        ]

    def test_fresh_publish_dry_runs_publishes_and_waits_for_the_index(self):
        self.write_state(publish_delays={self.spec: 2})

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        publishes = [call for call in self.cargo_calls() if call[0] == "publish"]
        self.assertEqual(
            publishes,
            [
                ["publish", "--locked", "-p", CRATE, "--dry-run"],
                ["publish", "--locked", "-p", CRATE],
            ],
        )
        # One preflight query, then polls until the third shows the version.
        self.assertEqual(self.index_requests, [index_path(CRATE)] * 4)
        self.assertIn("verified: {} cksum {}".format(self.spec, self.local_cksum), result.stdout)
        self.assertNotIn("fake-token", result.stdout + result.stderr)

    def test_identical_existing_version_is_skipped(self):
        self.write_state(index={self.spec: self.index_record(self.local_cksum)})

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.real_publishes(), [])
        self.assertIn("skip: " + self.spec, result.stdout)

    def test_different_existing_version_is_refused(self):
        other = cksum(b"other bytes")
        self.write_state(index={self.spec: self.index_record(other)})

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.real_publishes(), [])
        self.assertIn(self.local_cksum, result.stderr)
        self.assertIn(other, result.stderr)

    def test_yanked_version_is_refused(self):
        self.write_state(index={self.spec: self.index_record(self.local_cksum, yanked=True)})

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.real_publishes(), [])
        self.assertIn("yanked", result.stderr)

    def test_registry_failures_stop_before_publish(self):
        for mode in ("error", "malformed"):
            with self.subTest(mode=mode):
                self.write_state(
                    index_mode=mode,
                    index={self.spec: self.index_record(self.local_cksum)} if mode == "malformed" else {},
                )

                result = self.run_script()

                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.real_publishes(), [])
                self.assertIn(self.spec, result.stderr)

    def test_dry_run_failure_stops_before_publish(self):
        self.write_state(dry_run_fail=[self.spec])

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.real_publishes(), [])
        self.assertIn("--dry-run failed for " + self.spec, result.stderr)

    def test_visibility_timeout_fails_loudly_and_rerun_resumes(self):
        self.write_state(publish_delays={self.spec: 10})

        timed_out = self.run_script(poll_attempts="3", poll_interval="0.01")

        self.assertNotEqual(timed_out.returncode, 0)
        self.assertIn("after 3 attempts", timed_out.stderr)
        self.assertIn("Rerun", timed_out.stderr)

        state = self.read_state()
        state["index"][self.spec] = state["pending"].pop(self.spec)["record"]
        self.write_state(**state)
        resumed = self.run_script(poll_attempts="3", poll_interval="0")

        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertEqual(self.real_publishes(), [])
        self.assertIn("skip: " + self.spec, resumed.stdout)

    def test_index_serving_other_bytes_after_publish_fails(self):
        served = cksum(b"tampered")
        self.write_state(served_cksums={self.spec: served})

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(served, result.stderr)

    def test_dirty_tree_and_unpublishable_package_are_refused_before_publish(self):
        (self.checkout / "README").write_text("changed\n", encoding="utf-8")
        dirty = self.run_script()
        self.assertNotEqual(dirty.returncode, 0)
        self.assertIn("not clean", dirty.stderr)
        self.assertEqual(self.cargo_calls(), [])

        subprocess.run(["git", "-C", str(self.checkout), "checkout", "-q", "README"], check=True)
        unpublishable = self.run_script(package="stemma-api")
        self.assertNotEqual(unpublishable.returncode, 0)
        self.assertIn("publish = false", unpublishable.stderr)
        self.assertEqual(self.real_publishes(), [])


def job_block(workflow, name):
    match = re.search(r"^  {}:\n".format(re.escape(name)), workflow, re.MULTILINE)
    if match is None:
        raise AssertionError("release.yml has no job named " + name)
    following = re.search(r"^  [a-z0-9-]+:\n", workflow[match.end():], re.MULTILINE)
    end = match.end() + following.start() if following else len(workflow)
    return workflow[match.start():end]


def needs_of(block):
    match = re.search(r"^    needs: (?:\[(?P<list>[^\]]*)\]|(?P<one>[a-z0-9-]+))$", block, re.MULTILINE)
    if match is None:
        return set()
    if match.group("one"):
        return {match.group("one")}
    return {job.strip() for job in match.group("list").split(",")}


class CratesReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text(encoding="utf-8")
        self.job = job_block(self.workflow, "crates-publish")

    def ancestors(self, name):
        seen = set()
        frontier = [name]
        while frontier:
            for parent in needs_of(job_block(self.workflow, frontier.pop())):
                if parent not in seen:
                    seen.add(parent)
                    frontier.append(parent)
        return seen

    def test_crates_publish_runs_only_after_approval_and_tag_binding(self):
        ancestors = self.ancestors("crates-publish")
        self.assertIn("release-approval", ancestors)
        self.assertIn("release-claim", ancestors)
        self.assertIn("crates-publish", self.ancestors("github-release"))

    def test_trusted_publishing_without_stored_token(self):
        self.assertIn("id-token: write", self.job)
        self.assertNotIn("secrets.", self.job)
        self.assertNotIn("environment:", self.job)

    def test_each_crate_gets_its_own_token_in_dependency_order(self):
        steps = re.findall(
            r"uses: rust-lang/crates-io-auth-action@v1\n\s+id: (?P<id>[a-z0-9-]+)\n"
            r"\s+- name: [^\n]+\n"
            r"\s+env:\n\s+CARGO_REGISTRY_TOKEN: \$\{\{ steps\.(?P<used>[a-z0-9-]+)\.outputs\.token \}\}\n"
            r"\s+run: bash scripts/publish-crate\.sh (?P<crate>[a-z-]+)\n",
            self.job,
        )
        self.assertEqual([crate for _id, _used, crate in steps], list(CRATE_ORDER))
        for step_id, used, crate in steps:
            self.assertEqual(step_id, used, crate)
        self.assertEqual(self.job.count("CARGO_REGISTRY_TOKEN"), len(CRATE_ORDER))

    def test_window_and_toolchain_are_explicit(self):
        self.assertIn('STEMMA_CRATES_PUBLISH_POLL_ATTEMPTS: "60"', self.job)
        self.assertIn('STEMMA_CRATES_PUBLISH_POLL_INTERVAL_SECONDS: "10"', self.job)
        self.assertIn(".mise.toml", self.job)
        self.assertIn("toolchain: ${{ steps.toolchain.outputs.version }}", self.job)
        self.assertNotIn("rust-toolchain@stable", self.job)

    def test_manual_recipe_uses_the_same_script_in_the_same_order(self):
        justfile = JUSTFILE.read_text(encoding="utf-8")
        recipe = re.search(r"^publish-crates:.*\n((?:    .*\n)+)", justfile, re.MULTILINE)
        self.assertIsNotNone(recipe)
        self.assertEqual(
            re.findall(r"bash scripts/publish-crate\.sh ([a-z-]+)", recipe.group(1)),
            list(CRATE_ORDER),
        )


if __name__ == "__main__":
    unittest.main()
