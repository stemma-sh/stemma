#!/usr/bin/env python3
"""Validate a downloaded native matrix and freeze a release candidate manifest.

Usage:
    python3 stemma-mcp/build_candidate_manifest.py \
        binaries 0.2.0 0123456789abcdef0123456789abcdef01234567 \
        candidate-manifest.json

The input directory must be the direct output of downloading the five named
GitHub Actions artifacts. Validation finishes before the output is created,
and an existing output is never replaced.
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import struct
import sys
from pathlib import Path


MANIFEST_SCHEMA = "stemma.release.candidate_manifest/v2"
REPORT_SUITE = "stemma.mcp.safe_artifact_conformance/v1"
REPORT_TOOL_PROFILE = "advanced"
REPORT_NAME = "safe-artifact-conformance.json"
REPORT_CASE_COUNT = 21
MAX_REPORT_BYTES = 5 * 1024 * 1024

TARGETS = (
    (
        "x86_64-unknown-linux-gnu",
        {
            "binary": "stemma-mcp",
            "format": "elf",
            "architecture": "x86_64",
            "system": "Linux",
        },
    ),
    (
        "aarch64-unknown-linux-gnu",
        {
            "binary": "stemma-mcp",
            "format": "elf",
            "architecture": "aarch64",
            "system": "Linux",
        },
    ),
    (
        "x86_64-apple-darwin",
        {
            "binary": "stemma-mcp",
            "format": "mach-o",
            "architecture": "x86_64",
            "system": "Darwin",
        },
    ),
    (
        "aarch64-apple-darwin",
        {
            "binary": "stemma-mcp",
            "format": "mach-o",
            "architecture": "aarch64",
            "system": "Darwin",
        },
    ),
    (
        "x86_64-pc-windows-msvc",
        {
            "binary": "stemma-mcp.exe",
            "format": "pe",
            "architecture": "x86_64",
            "system": "Windows",
        },
    ),
)

REPORT_CASE_IDS = frozenset(
    {
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
    }
)

SHA_RE = re.compile(r"[0-9a-f]{40}")
VERSION_RE = re.compile(
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)"
)
PYTHON_VERSION_RE = re.compile(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?(?:[-+].*)?")


class ManifestError(Exception):
    """A candidate cannot be proven complete and internally consistent."""


class DuplicateJsonKey(ValueError):
    """A JSON object used an ambiguous duplicate member name."""


def require(condition, message):
    if not condition:
        raise ManifestError(message)


def utc_now():
    return (
        datetime.datetime.now(datetime.timezone.utc)
        .isoformat()
        .replace("+00:00", "Z")
    )


def file_identity(path):
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJsonKey("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result


def reject_nonfinite(value):
    raise ValueError("non-finite JSON number {}".format(value))


def parse_report(report_bytes, path):
    try:
        text = report_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ManifestError("{} is not UTF-8: {}".format(path, error))
    try:
        return json.loads(
            text,
            object_pairs_hook=unique_object,
            parse_constant=reject_nonfinite,
        )
    except (json.JSONDecodeError, DuplicateJsonKey, ValueError) as error:
        raise ManifestError("{} is not strict JSON: {}".format(path, error))


def validate_timestamp(value, field):
    require(
        isinstance(value, str) and value.endswith("Z"),
        "{} must be an ISO-8601 UTC timestamp ending in Z".format(field),
    )
    try:
        parsed = datetime.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise ManifestError("{} is not a valid timestamp: {}".format(field, error))
    require(
        parsed.utcoffset() == datetime.timedelta(0),
        "{} is not UTC".format(field),
    )
    return parsed


def normalize_machine(value):
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower().replace("-", "_")
    if normalized in {"x86_64", "amd64", "x64"}:
        return "x86_64"
    if normalized in {"aarch64", "arm64"}:
        return "aarch64"
    return None


def inspect_executable(path):
    """Return the executable container and CPU from its native header."""
    with path.open("rb") as handle:
        header = handle.read(4096)
        require(len(header) >= 20, "{} is too small to be a native binary".format(path))

        if header.startswith(b"\x7fELF"):
            require(header[4] == 2, "{} is not a 64-bit ELF binary".format(path))
            byte_order = header[5]
            require(byte_order in (1, 2), "{} has an invalid ELF byte order".format(path))
            endian = "<" if byte_order == 1 else ">"
            machine = struct.unpack_from(endian + "H", header, 18)[0]
            architectures = {0x3E: "x86_64", 0xB7: "aarch64"}
            require(machine in architectures, "{} has unsupported ELF machine {}".format(path, machine))
            return {"format": "elf", "architecture": architectures[machine]}

        if header[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
            endian = "<" if header[:4] == b"\xcf\xfa\xed\xfe" else ">"
            cpu_type = struct.unpack_from(endian + "I", header, 4)[0]
            architectures = {0x01000007: "x86_64", 0x0100000C: "aarch64"}
            require(
                cpu_type in architectures,
                "{} has unsupported Mach-O CPU type {}".format(path, cpu_type),
            )
            return {"format": "mach-o", "architecture": architectures[cpu_type]}

        if header.startswith(b"MZ"):
            require(len(header) >= 64, "{} has a truncated DOS header".format(path))
            pe_offset = struct.unpack_from("<I", header, 0x3C)[0]
            handle.seek(pe_offset)
            pe_header = handle.read(6)
            require(
                len(pe_header) == 6 and pe_header[:4] == b"PE\x00\x00",
                "{} has an invalid PE header".format(path),
            )
            machine = struct.unpack_from("<H", pe_header, 4)[0]
            require(machine == 0x8664, "{} is not an x86-64 PE binary".format(path))
            return {"format": "pe", "architecture": "x86_64"}

    raise ManifestError("{} is not an ELF, thin 64-bit Mach-O, or PE binary".format(path))


def validate_platform(platform_value, spec, target):
    require(isinstance(platform_value, dict), "{} report platform must be an object".format(target))
    system = platform_value.get("system")
    require(
        isinstance(system, str) and system.casefold() == spec["system"].casefold(),
        "{} report system {!r} does not match {}".format(target, system, spec["system"]),
    )
    machine = normalize_machine(platform_value.get("machine"))
    require(
        machine == spec["architecture"],
        "{} report machine {!r} does not match {}".format(
            target, platform_value.get("machine"), spec["architecture"]
        ),
    )
    release = platform_value.get("release")
    require(
        isinstance(release, str) and 0 < len(release) <= 256,
        "{} report platform.release is missing or unreasonable".format(target),
    )
    python_version = platform_value.get("python")
    require(
        isinstance(python_version, str)
        and PYTHON_VERSION_RE.fullmatch(python_version) is not None,
        "{} report Python version is missing or unreasonable".format(target),
    )
    return {
        "machine": platform_value["machine"],
        "python": python_version,
        "release": release,
        "system": system,
    }


def validate_report(report, target, spec, binary_identity, expected_server_version):
    require(isinstance(report, dict), "{} conformance report must be an object".format(target))
    require(
        report.get("suite") == REPORT_SUITE,
        "{} conformance suite is not {}".format(target, REPORT_SUITE),
    )
    require(
        report.get("tool_profile") == REPORT_TOOL_PROFILE,
        "{} conformance tool_profile is not {}".format(
            target, REPORT_TOOL_PROFILE
        ),
    )
    require(report.get("ok") is True, "{} conformance report is not passing".format(target))
    require(
        report.get("server_version") == expected_server_version,
        "{} server_version does not match {}".format(target, expected_server_version),
    )
    require(
        report.get("binary_identity") == binary_identity,
        "{} binary_identity does not match the exact downloaded binary".format(target),
    )

    require(
        type(report.get("case_count")) is int
        and report["case_count"] == REPORT_CASE_COUNT,
        "{} case_count is not {}".format(target, REPORT_CASE_COUNT),
    )
    require(
        type(report.get("mandatory_case_count")) is int
        and report["mandatory_case_count"] == REPORT_CASE_COUNT,
        "{} mandatory_case_count is not {}".format(target, REPORT_CASE_COUNT),
    )
    counts = report.get("counts")
    require(isinstance(counts, dict), "{} report counts must be an object".format(target))
    require(
        type(counts.get("passed")) is int and counts["passed"] == REPORT_CASE_COUNT,
        "{} did not pass all {} cases".format(target, REPORT_CASE_COUNT),
    )
    require(
        type(counts.get("failed")) is int and counts["failed"] == 0,
        "{} has failed conformance cases".format(target),
    )
    require(
        type(counts.get("blocked")) is int and counts["blocked"] == 0,
        "{} has blocked mandatory conformance cases".format(target),
    )

    cases = report.get("cases")
    require(
        isinstance(cases, list) and len(cases) == REPORT_CASE_COUNT,
        "{} report does not contain exactly {} case receipts".format(target, REPORT_CASE_COUNT),
    )
    case_ids = []
    for case in cases:
        require(isinstance(case, dict), "{} contains a non-object case receipt".format(target))
        require(case.get("mandatory") is True, "{} contains a non-mandatory case".format(target))
        require(case.get("status") == "passed", "{} contains a non-passing case".format(target))
        require(isinstance(case.get("id"), str), "{} contains a case without an id".format(target))
        case_ids.append(case["id"])
    require(
        len(set(case_ids)) == REPORT_CASE_COUNT and set(case_ids) == REPORT_CASE_IDS,
        "{} report case ids do not match the stable v1 case set".format(target),
    )

    started_value = report.get("started_at")
    finished_value = report.get("finished_at")
    started = validate_timestamp(started_value, "{}.started_at".format(target))
    finished = validate_timestamp(finished_value, "{}.finished_at".format(target))
    require(finished >= started, "{} report finished before it started".format(target))
    platform_value = validate_platform(report.get("platform"), spec, target)
    return {
        "finished_at": finished_value,
        "platform": platform_value,
        "started_at": started_value,
    }


def require_regular_file(path, label):
    require(path.exists(), "missing {}: {}".format(label, path))
    require(not path.is_symlink(), "{} must not be a symlink: {}".format(label, path))
    require(path.is_file(), "{} must be a regular file: {}".format(label, path))


def exact_directory_entries(path):
    try:
        return {entry.name: entry for entry in path.iterdir()}
    except OSError as error:
        raise ManifestError("cannot inventory {}: {}".format(path, error))


def validate_layout(artifacts_root):
    require(artifacts_root.exists(), "artifacts directory does not exist: {}".format(artifacts_root))
    require(not artifacts_root.is_symlink(), "artifacts directory must not be a symlink")
    require(artifacts_root.is_dir(), "artifacts input is not a directory: {}".format(artifacts_root))
    entries = exact_directory_entries(artifacts_root)
    expected = {target for target, _spec in TARGETS}
    actual = set(entries)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    require(not missing, "missing native target directories: {}".format(", ".join(missing)))
    require(not extra, "unexpected entries in artifacts directory: {}".format(", ".join(extra)))
    for target, _spec in TARGETS:
        target_dir = entries[target]
        require(not target_dir.is_symlink(), "target directory must not be a symlink: {}".format(target))
        require(target_dir.is_dir(), "target entry is not a directory: {}".format(target))


def validate_target(artifacts_root, target, spec, expected_server_version):
    target_dir = artifacts_root / target
    entries = exact_directory_entries(target_dir)
    expected_names = {spec["binary"], REPORT_NAME}
    missing = sorted(expected_names - set(entries))
    extra = sorted(set(entries) - expected_names)
    require(not missing, "{} is missing: {}".format(target, ", ".join(missing)))
    require(not extra, "{} has unexpected entries: {}".format(target, ", ".join(extra)))

    binary_path = entries[spec["binary"]]
    report_path = entries[REPORT_NAME]
    require_regular_file(binary_path, "{} binary".format(target))
    require_regular_file(report_path, "{} report".format(target))

    binary_identity = file_identity(binary_path)
    require(binary_identity["bytes"] > 0, "{} binary is empty".format(target))
    executable = inspect_executable(binary_path)
    require(
        executable["format"] == spec["format"]
        and executable["architecture"] == spec["architecture"],
        "{} binary header is {}/{}, expected {}/{}".format(
            target,
            executable["format"],
            executable["architecture"],
            spec["format"],
            spec["architecture"],
        ),
    )

    report_size = report_path.stat().st_size
    require(report_size > 0, "{} conformance report is empty".format(target))
    require(
        report_size <= MAX_REPORT_BYTES,
        "{} conformance report exceeds {} bytes".format(target, MAX_REPORT_BYTES),
    )
    report_bytes = report_path.read_bytes()
    require(
        len(report_bytes) <= MAX_REPORT_BYTES,
        "{} conformance report grew beyond {} bytes while reading".format(
            target, MAX_REPORT_BYTES
        ),
    )
    report_identity = {
        "bytes": len(report_bytes),
        "sha256": hashlib.sha256(report_bytes).hexdigest(),
    }
    report = parse_report(report_bytes, report_path)
    validated = validate_report(
        report, target, spec, binary_identity, expected_server_version
    )

    manifest_entry = {
        "binary": {
            "architecture": executable["architecture"],
            "bytes": binary_identity["bytes"],
            "format": executable["format"],
            "name": spec["binary"],
            "sha256": binary_identity["sha256"],
        },
        "platform": validated["platform"],
        "qualification": {
            "finished_at": validated["finished_at"],
            "mandatory_cases": REPORT_CASE_COUNT,
            "passed_cases": REPORT_CASE_COUNT,
            "started_at": validated["started_at"],
            "suite": REPORT_SUITE,
            "tool_profile": REPORT_TOOL_PROFILE,
        },
        "report": {
            "bytes": report_identity["bytes"],
            "name": REPORT_NAME,
            "sha256": report_identity["sha256"],
        },
        "target": target,
    }
    return manifest_entry, binary_path, binary_identity, report_path, report_identity


def build_manifest(artifacts_root, version, source_sha):
    require(
        VERSION_RE.fullmatch(version) is not None,
        "version must be stable MAJOR.MINOR.PATCH SemVer",
    )
    require(SHA_RE.fullmatch(source_sha) is not None, "source SHA must be 40 lowercase hexadecimal characters")
    expected_server_version = "{}+g{}".format(version, source_sha[:12])
    validate_layout(artifacts_root)

    entries = []
    identities = []
    for target, spec in TARGETS:
        entry, binary_path, binary_identity, report_path, report_identity = validate_target(
            artifacts_root, target, spec, expected_server_version
        )
        entries.append(entry)
        identities.append((binary_path, binary_identity))
        identities.append((report_path, report_identity))

    # Re-inventory and re-hash after validation so a concurrently changed
    # download cannot be frozen under an identity checked from earlier bytes.
    validate_layout(artifacts_root)
    for target, spec in TARGETS:
        target_dir = artifacts_root / target
        target_entries = exact_directory_entries(target_dir)
        expected_names = {spec["binary"], REPORT_NAME}
        require(
            set(target_entries) == expected_names,
            "{} contents changed during aggregation".format(target),
        )
        require_regular_file(
            target_entries[spec["binary"]], "{} binary".format(target)
        )
        require_regular_file(
            target_entries[REPORT_NAME], "{} report".format(target)
        )
    for path, identity in identities:
        require(
            file_identity(path) == identity,
            "candidate input changed during aggregation: {}".format(path),
        )

    return {
        "build": {
            "server_version": expected_server_version,
            "version": version,
        },
        "generated_at": utc_now(),
        "schema": MANIFEST_SCHEMA,
        "source": {"git_sha": source_sha},
        "targets": entries,
    }


def write_create_new(output_path, manifest):
    require(
        not output_path.exists() and not output_path.is_symlink(),
        "refusing to replace existing output: {}".format(output_path),
    )
    parent = output_path.parent if output_path.parent != Path("") else Path(".")
    require(parent.exists() and parent.is_dir(), "output parent is not a directory: {}".format(parent))
    content = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
    try:
        descriptor = os.open(os.fspath(output_path), flags, 0o644)
    except FileExistsError:
        raise ManifestError("refusing to replace existing output: {}".format(output_path))
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            descriptor = None
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
    except Exception:
        if descriptor is not None:
            os.close(descriptor)
        try:
            output_path.unlink()
        except OSError:
            pass
        raise


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", help="downloaded Actions artifacts directory")
    parser.add_argument("version", help="requested release version without v prefix")
    parser.add_argument("source_sha", help="full 40-character release source SHA")
    parser.add_argument("output", help="new candidate manifest path")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    artifacts_root = Path(args.artifacts).expanduser()
    output_path = Path(args.output).expanduser()
    try:
        require(
            not output_path.exists() and not output_path.is_symlink(),
            "refusing to replace existing output: {}".format(output_path),
        )
        manifest = build_manifest(artifacts_root, args.version, args.source_sha)
        write_create_new(output_path, manifest)
    except (ManifestError, OSError) as error:
        print("error: {}".format(error), file=sys.stderr)
        return 1
    print("wrote {}".format(output_path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
