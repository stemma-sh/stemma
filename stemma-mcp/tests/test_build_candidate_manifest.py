#!/usr/bin/env python3
"""Hermetic tests for the exact-candidate manifest aggregator."""

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


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "stemma-mcp" / "build_candidate_manifest.py"
VERSION = "1.2.3"
SOURCE_SHA = "0123456789abcdef0123456789abcdef01234567"
SERVER_VERSION = VERSION + "+g" + SOURCE_SHA[:12]
REPORT_NAME = "safe-artifact-conformance.json"
SUITE = "stemma.mcp.safe_artifact_conformance/v1"

TARGETS = (
    (
        "x86_64-unknown-linux-gnu",
        "stemma-mcp",
        "Linux",
        "x86_64",
        "elf",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "stemma-mcp",
        "Linux",
        "aarch64",
        "elf",
    ),
    (
        "x86_64-apple-darwin",
        "stemma-mcp",
        "Darwin",
        "x86_64",
        "mach-o",
    ),
    (
        "aarch64-apple-darwin",
        "stemma-mcp",
        "Darwin",
        "arm64",
        "mach-o",
    ),
    (
        "x86_64-pc-windows-msvc",
        "stemma-mcp.exe",
        "Windows",
        "AMD64",
        "pe",
    ),
)

CASE_IDS = (
    "open.relative_in_root",
    "open.absolute_in_root",
    "open.parent_escape_refused",
    "open.absolute_outside_refused",
    "open.symlink_escape_refused",
    "save.fresh_identity",
    "save.existing_collision_preserved",
    "save.protected_source_refused",
    "save.windows_stream_syntax_refused",
    "save.hardlink_alias_refused",
    "save.outside_refused",
    "compare.fresh_identity",
    "compare.protected_source_refused",
    "compare.existing_collision_preserved",
    "compare.outside_refused",
    "audit.render_fresh_identity",
    "audit.render_collision_preserved",
    "audit.render_outside_refused",
    "review.render_fresh_identity",
    "review.render_collision_preserved",
    "review.render_outside_refused",
)


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def fake_elf(architecture):
    data = bytearray(128)
    data[:6] = b"\x7fELF\x02\x01"
    machine = 0x3E if architecture == "x86_64" else 0xB7
    struct.pack_into("<H", data, 18, machine)
    data[64:] = ("fake-{}-elf".format(architecture).encode("ascii") * 8)[:64]
    return bytes(data)


def fake_macho(architecture):
    data = bytearray(128)
    data[:4] = b"\xcf\xfa\xed\xfe"
    cpu_type = 0x01000007 if architecture == "x86_64" else 0x0100000C
    struct.pack_into("<I", data, 4, cpu_type)
    data[64:] = ("fake-{}-macho".format(architecture).encode("ascii") * 8)[:64]
    return bytes(data)


def fake_pe():
    data = bytearray(256)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\x00\x00"
    struct.pack_into("<H", data, 0x84, 0x8664)
    data[160:] = (b"fake-x86-64-pe" * 8)[:96]
    return bytes(data)


def fake_binary(format_name, machine):
    architecture = "aarch64" if machine in ("aarch64", "arm64") else "x86_64"
    if format_name == "elf":
        return fake_elf(architecture)
    if format_name == "mach-o":
        return fake_macho(architecture)
    return fake_pe()


class CandidateManifestTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(
            prefix="stemma-candidate-manifest-test-"
        )
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.artifacts = self.root / "artifacts"
        self.output = self.root / "candidate-manifest.json"
        self.artifacts.mkdir()
        self._build_complete_matrix()

    def _build_complete_matrix(self):
        for index, (target, binary_name, system, machine, format_name) in enumerate(
            TARGETS
        ):
            target_dir = self.artifacts / target
            target_dir.mkdir()
            binary = fake_binary(format_name, machine)
            (target_dir / binary_name).write_bytes(binary)
            start = datetime.datetime(
                2026, 7, 12, 20, index, 0, tzinfo=datetime.timezone.utc
            )
            finish = start + datetime.timedelta(seconds=30)
            report = {
                "binary": binary_name,
                "binary_identity": {
                    "bytes": len(binary),
                    "sha256": sha256_bytes(binary),
                },
                "case_count": 21,
                "cases": [
                    {"id": case_id, "mandatory": True, "status": "passed"}
                    for case_id in CASE_IDS
                ],
                "counts": {"blocked": 0, "failed": 0, "passed": 21},
                "finished_at": finish.isoformat().replace("+00:00", "Z"),
                "mandatory_case_count": 21,
                "ok": True,
                "platform": {
                    "machine": machine,
                    "python": "3.11.9",
                    "release": "test-kernel-1",
                    "system": system,
                },
                "server_version": SERVER_VERSION,
                "started_at": start.isoformat().replace("+00:00", "Z"),
                "suite": SUITE,
                "tool_profile": "advanced",
            }
            (target_dir / REPORT_NAME).write_text(
                json.dumps(report, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )

    def _run(self, output=None, version=VERSION):
        output = output or self.output
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                str(self.artifacts),
                version,
                SOURCE_SHA,
                str(output),
            ],
            cwd=str(REPO_ROOT),
            text=True,
            capture_output=True,
            check=False,
        )

    def _load_report(self, target):
        path = self.artifacts / target / REPORT_NAME
        return path, json.loads(path.read_text(encoding="utf-8"))

    def test_complete_matrix_emits_content_minimized_manifest(self):
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest_text = self.output.read_text(encoding="utf-8")
        manifest = json.loads(manifest_text)
        self.assertEqual(manifest["schema"], "stemma.release.candidate_manifest/v2")
        self.assertEqual(manifest["source"], {"git_sha": SOURCE_SHA})
        self.assertEqual(
            manifest["build"],
            {"server_version": SERVER_VERSION, "version": VERSION},
        )
        self.assertTrue(manifest["generated_at"].endswith("Z"))
        self.assertNotIn('"cases"', manifest_text)
        self.assertNotIn(str(self.artifacts), manifest_text)
        self.assertEqual(
            [entry["target"] for entry in manifest["targets"]],
            [target[0] for target in TARGETS],
        )

        for entry, target_spec in zip(manifest["targets"], TARGETS):
            target, binary_name, system, machine, format_name = target_spec
            binary_path = self.artifacts / target / binary_name
            report_path = self.artifacts / target / REPORT_NAME
            binary = binary_path.read_bytes()
            report = report_path.read_bytes()
            self.assertEqual(entry["binary"]["sha256"], sha256_bytes(binary))
            self.assertEqual(entry["binary"]["bytes"], len(binary))
            self.assertEqual(entry["binary"]["format"], format_name)
            self.assertEqual(entry["report"]["sha256"], sha256_bytes(report))
            self.assertEqual(entry["report"]["bytes"], len(report))
            self.assertEqual(entry["platform"]["system"], system)
            self.assertEqual(entry["platform"]["machine"], machine)
            self.assertEqual(entry["qualification"]["passed_cases"], 21)
            self.assertEqual(
                entry["qualification"]["tool_profile"], "advanced"
            )

    def test_prerelease_and_build_metadata_versions_are_rejected(self):
        for version in ("1.2.3-rc.1", "1.2.3+qualified.1"):
            with self.subTest(version=version):
                output = self.root / (
                    "candidate-" + version.replace("+", "_") + ".json"
                )
                result = self._run(output=output, version=version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("stable MAJOR.MINOR.PATCH", result.stderr)
                self.assertFalse(output.exists())

    def test_binary_tamper_is_rejected_without_output(self):
        target, binary_name, _system, _machine, _format_name = TARGETS[0]
        with (self.artifacts / target / binary_name).open("ab") as handle:
            handle.write(b"tampered")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("binary_identity", result.stderr)
        self.assertFalse(self.output.exists())

    def test_failed_report_tamper_is_rejected_without_output(self):
        report_path, report = self._load_report(TARGETS[0][0])
        report["counts"] = {"blocked": 0, "failed": 1, "passed": 19}
        report["ok"] = False
        report_path.write_text(json.dumps(report), encoding="utf-8")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not passing", result.stderr)
        self.assertFalse(self.output.exists())

    def test_wrong_or_missing_tool_profile_is_rejected_without_output(self):
        target = TARGETS[0][0]
        report_path, report = self._load_report(target)
        for tool_profile in (None, "core"):
            with self.subTest(tool_profile=tool_profile):
                if tool_profile is None:
                    report.pop("tool_profile", None)
                else:
                    report["tool_profile"] = tool_profile
                report_path.write_text(json.dumps(report), encoding="utf-8")
                result = self._run()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("tool_profile is not advanced", result.stderr)
                self.assertFalse(self.output.exists())
                report["tool_profile"] = "advanced"

    def test_missing_target_is_rejected(self):
        missing_target = TARGETS[1][0]
        shutil.rmtree(self.artifacts / missing_target)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(missing_target, result.stderr)
        self.assertFalse(self.output.exists())

    def test_missing_target_member_is_rejected(self):
        target = TARGETS[2][0]
        (self.artifacts / target / REPORT_NAME).unlink()
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(REPORT_NAME, result.stderr)
        self.assertFalse(self.output.exists())

    def test_extra_target_is_rejected(self):
        (self.artifacts / "unexpected-target").mkdir()
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected-target", result.stderr)
        self.assertFalse(self.output.exists())

    def test_extra_target_member_is_rejected(self):
        target = TARGETS[3][0]
        (self.artifacts / target / "unqualified-copy").write_bytes(b"extra")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unqualified-copy", result.stderr)
        self.assertFalse(self.output.exists())

    def test_platform_architecture_mismatch_is_rejected(self):
        target = TARGETS[1][0]
        report_path, report = self._load_report(target)
        report["platform"]["machine"] = "x86_64"
        report_path.write_text(json.dumps(report), encoding="utf-8")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("report machine", result.stderr)
        self.assertFalse(self.output.exists())

    def test_existing_output_is_preserved(self):
        original = b"existing-candidate-manifest"
        self.output.write_bytes(original)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to replace", result.stderr)
        self.assertEqual(self.output.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
