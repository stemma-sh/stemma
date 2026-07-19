#!/usr/bin/env python3
"""Hermetic tests for downstream candidate-manifest verification."""

import datetime
import hashlib
import json
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_DIR = REPO_ROOT / "stemma-mcp"
SCRIPT = SCRIPT_DIR / "verify_candidate_manifest.py"
sys.dont_write_bytecode = True
sys.path.insert(0, str(SCRIPT_DIR))
import build_candidate_manifest as builder  # noqa: E402
import verify_candidate_manifest as verifier  # noqa: E402


VERSION = "1.2.3"
SOURCE_SHA = "0123456789abcdef0123456789abcdef01234567"
SERVER_VERSION = VERSION + "+g" + SOURCE_SHA[:12]


def sha256_bytes(content):
    return hashlib.sha256(content).hexdigest()


def fake_elf(architecture):
    content = bytearray(128)
    content[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", content, 18, 0x3E if architecture == "x86_64" else 0xB7)
    content[64:] = ("fake-{}-elf".format(architecture).encode("ascii") * 8)[:64]
    return bytes(content)


def fake_macho(architecture):
    content = bytearray(128)
    content[:4] = b"\xcf\xfa\xed\xfe"
    cpu_type = 0x01000007 if architecture == "x86_64" else 0x0100000C
    struct.pack_into("<I", content, 4, cpu_type)
    content[64:] = ("fake-{}-macho".format(architecture).encode("ascii") * 8)[:64]
    return bytes(content)


def fake_pe():
    content = bytearray(256)
    content[:2] = b"MZ"
    struct.pack_into("<I", content, 0x3C, 0x80)
    content[0x80:0x84] = b"PE\x00\x00"
    struct.pack_into("<H", content, 0x84, 0x8664)
    content[160:] = (b"fake-x86-64-pe" * 8)[:96]
    return bytes(content)


def fake_binary(format_name, architecture):
    if format_name == "elf":
        return fake_elf(architecture)
    if format_name == "mach-o":
        return fake_macho(architecture)
    return fake_pe()


def platform_machine(architecture, system):
    if architecture == "aarch64" and system == "Darwin":
        return "arm64"
    if architecture == "x86_64" and system == "Windows":
        return "AMD64"
    return architecture


class VerifyCandidateManifestTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(
            prefix="stemma-verify-candidate-test-"
        )
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "artifacts"
        self.root.mkdir()
        self._build_downloaded_artifacts()

    @property
    def manifest_path(self):
        return (
            self.root
            / verifier.CANDIDATE_ARTIFACT
            / verifier.CANDIDATE_MANIFEST
        )

    def _build_downloaded_artifacts(self):
        now = datetime.datetime.now(datetime.timezone.utc)
        for index, (target, spec) in enumerate(builder.TARGETS):
            target_dir = self.root / target
            target_dir.mkdir()
            binary = fake_binary(spec["format"], spec["architecture"])
            (target_dir / spec["binary"]).write_bytes(binary)
            started = now - datetime.timedelta(minutes=10) + datetime.timedelta(
                seconds=index
            )
            finished = started + datetime.timedelta(minutes=1)
            report = {
                "binary": spec["binary"],
                "binary_identity": {
                    "bytes": len(binary),
                    "sha256": sha256_bytes(binary),
                },
                "case_count": builder.REPORT_CASE_COUNT,
                "cases": [
                    {"id": case_id, "mandatory": True, "status": "passed"}
                    for case_id in sorted(builder.REPORT_CASE_IDS)
                ],
                "counts": {
                    "blocked": 0,
                    "failed": 0,
                    "passed": builder.REPORT_CASE_COUNT,
                },
                "finished_at": finished.isoformat().replace("+00:00", "Z"),
                "mandatory_case_count": builder.REPORT_CASE_COUNT,
                "ok": True,
                "platform": {
                    "machine": platform_machine(
                        spec["architecture"], spec["system"]
                    ),
                    "python": "3.11.9",
                    "release": "test-kernel-1",
                    "system": spec["system"],
                },
                "server_version": SERVER_VERSION,
                "started_at": started.isoformat().replace("+00:00", "Z"),
                "suite": builder.REPORT_SUITE,
            }
            (target_dir / builder.REPORT_NAME).write_text(
                json.dumps(report, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )

        manifest = builder.build_manifest(self.root, VERSION, SOURCE_SHA)
        manifest_dir = self.root / verifier.CANDIDATE_ARTIFACT
        manifest_dir.mkdir()
        builder.write_create_new(
            manifest_dir / verifier.CANDIDATE_MANIFEST, manifest
        )

    def _run(self, version=VERSION, source_sha=SOURCE_SHA):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                str(self.root),
                version,
                source_sha,
            ],
            cwd=str(REPO_ROOT),
            text=True,
            capture_output=True,
            check=False,
        )

    def _manifest(self):
        return json.loads(self.manifest_path.read_text(encoding="utf-8"))

    def _write_manifest(self, manifest):
        self.manifest_path.write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def _tree_snapshot(self):
        return {
            str(path.relative_to(self.root)): sha256_bytes(path.read_bytes())
            for path in self.root.rglob("*")
            if path.is_file()
        }

    def test_valid_candidate_is_verified_without_mutation(self):
        before = self._tree_snapshot()
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("verified 5 native targets", result.stdout)
        self.assertEqual(self._tree_snapshot(), before)

    def test_every_frozen_field_is_compared(self):
        manifest = self._manifest()
        manifest["targets"][0]["binary"]["sha256"] = "0" * 64
        self._write_manifest(manifest)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("$.targets[0].binary.sha256", result.stderr)

    def test_binary_tamper_is_rejected(self):
        target, spec = builder.TARGETS[0]
        with (self.root / target / spec["binary"]).open("ab") as stream:
            stream.write(b"tampered")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("binary_identity", result.stderr)

    def test_missing_artifact_directory_is_rejected(self):
        shutil.rmtree(self.root / verifier.CANDIDATE_ARTIFACT)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(verifier.CANDIDATE_ARTIFACT, result.stderr)

    def test_extra_artifact_directory_is_rejected(self):
        (self.root / "unexpected-artifact").mkdir()
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected-artifact", result.stderr)

    def test_requested_version_and_sha_are_bound(self):
        version_result = self._run(version="1.2.4")
        self.assertNotEqual(version_result.returncode, 0)
        self.assertIn("build identity", version_result.stderr)
        other_sha = "f" * 40
        sha_result = self._run(source_sha=other_sha)
        self.assertNotEqual(sha_result.returncode, 0)
        self.assertIn("source identity", sha_result.stderr)

    def test_prerelease_and_build_metadata_requests_are_rejected(self):
        for version in ("1.2.3-rc.1", "1.2.3+qualified.1"):
            with self.subTest(version=version):
                result = self._run(version=version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("stable MAJOR.MINOR.PATCH", result.stderr)

    def test_schema_is_strict_and_duplicate_keys_are_rejected(self):
        content = self.manifest_path.read_text(encoding="utf-8")
        duplicate = content.replace(
            "{", '{"schema":"duplicate-schema",', 1
        )
        self.manifest_path.write_text(duplicate, encoding="utf-8")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate JSON key", result.stderr)

    def test_generated_at_must_be_valid_and_after_qualification(self):
        manifest = self._manifest()
        manifest["generated_at"] = "not-a-timestamp"
        self._write_manifest(manifest)
        invalid = self._run()
        self.assertNotEqual(invalid.returncode, 0)
        self.assertIn("ISO-8601 UTC", invalid.stderr)

        manifest = self._manifest()
        manifest["generated_at"] = "2000-01-01T00:00:00Z"
        self._write_manifest(manifest)
        early = self._run()
        self.assertNotEqual(early.returncode, 0)
        self.assertIn("before native qualification", early.stderr)

    def test_manifest_extra_field_is_rejected(self):
        manifest = self._manifest()
        manifest["unfrozen"] = True
        self._write_manifest(manifest)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected fields", result.stderr)

    def test_concurrent_input_change_is_caught_by_final_rehash(self):
        original_validate = verifier.builder.validate_target
        last_target = builder.TARGETS[-1][0]
        first_target, first_spec = builder.TARGETS[0]

        def validate_and_mutate(*args, **kwargs):
            result = original_validate(*args, **kwargs)
            if args[1] == last_target:
                with (self.root / first_target / first_spec["binary"]).open(
                    "ab"
                ) as stream:
                    stream.write(b"changed-after-validation")
            return result

        with mock.patch.object(
            verifier.builder, "validate_target", side_effect=validate_and_mutate
        ):
            with self.assertRaises(builder.ManifestError) as raised:
                verifier.verify_candidate(self.root, VERSION, SOURCE_SHA)
        self.assertIn("changed during verification", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
