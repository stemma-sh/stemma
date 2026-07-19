#!/usr/bin/env python3
"""Verify downloaded release artifacts against their frozen candidate manifest.

Usage:
    python3 stemma-mcp/verify_candidate_manifest.py \
        binaries 0.2.0 0123456789abcdef0123456789abcdef01234567

The verifier is read-only. The artifacts root must contain the five native
target artifacts and one ``candidate-manifest`` artifact downloaded from the
same workflow run.
"""

import argparse
import hashlib
import sys
from pathlib import Path


# Importing the freeze implementation must not create bytecode beside release
# tooling when this verifier is run from a read-only checkout.
sys.dont_write_bytecode = True
import build_candidate_manifest as builder  # noqa: E402


CANDIDATE_ARTIFACT = "candidate-manifest"
CANDIDATE_MANIFEST = "candidate-manifest.json"
MAX_MANIFEST_BYTES = 5 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise builder.ManifestError(message)


def require_regular_file(path, label):
    require(path.exists(), "missing {}: {}".format(label, path))
    require(not path.is_symlink(), "{} must not be a symlink: {}".format(label, path))
    require(path.is_file(), "{} must be a regular file: {}".format(label, path))


def exact_entries(path):
    try:
        return {entry.name: entry for entry in path.iterdir()}
    except OSError as error:
        raise builder.ManifestError("cannot inventory {}: {}".format(path, error))


def validate_download_layout(artifacts_root):
    require(artifacts_root.exists(), "artifacts directory does not exist: {}".format(artifacts_root))
    require(not artifacts_root.is_symlink(), "artifacts directory must not be a symlink")
    require(artifacts_root.is_dir(), "artifacts input is not a directory: {}".format(artifacts_root))

    root_entries = exact_entries(artifacts_root)
    expected_targets = {target for target, _spec in builder.TARGETS}
    expected_root = expected_targets | {CANDIDATE_ARTIFACT}
    missing = sorted(expected_root - set(root_entries))
    extra = sorted(set(root_entries) - expected_root)
    require(not missing, "missing downloaded artifact directories: {}".format(", ".join(missing)))
    require(not extra, "unexpected downloaded artifact entries: {}".format(", ".join(extra)))

    for target, spec in builder.TARGETS:
        target_dir = root_entries[target]
        require(not target_dir.is_symlink(), "target directory must not be a symlink: {}".format(target))
        require(target_dir.is_dir(), "target entry is not a directory: {}".format(target))
        target_entries = exact_entries(target_dir)
        expected_files = {spec["binary"], builder.REPORT_NAME}
        require(
            set(target_entries) == expected_files,
            "{} must contain exactly {}".format(
                target, ", ".join(sorted(expected_files))
            ),
        )
        require_regular_file(target_entries[spec["binary"]], "{} binary".format(target))
        require_regular_file(target_entries[builder.REPORT_NAME], "{} report".format(target))

    manifest_dir = root_entries[CANDIDATE_ARTIFACT]
    require(not manifest_dir.is_symlink(), "candidate-manifest directory must not be a symlink")
    require(manifest_dir.is_dir(), "candidate-manifest artifact is not a directory")
    manifest_entries = exact_entries(manifest_dir)
    require(
        set(manifest_entries) == {CANDIDATE_MANIFEST},
        "candidate-manifest artifact must contain exactly {}".format(CANDIDATE_MANIFEST),
    )
    require_regular_file(manifest_entries[CANDIDATE_MANIFEST], "candidate manifest")
    return manifest_entries[CANDIDATE_MANIFEST]


def strict_read_manifest(path):
    size = path.stat().st_size
    require(size > 0, "candidate manifest is empty")
    require(
        size <= MAX_MANIFEST_BYTES,
        "candidate manifest exceeds {} bytes".format(MAX_MANIFEST_BYTES),
    )
    content = path.read_bytes()
    require(
        len(content) <= MAX_MANIFEST_BYTES,
        "candidate manifest grew beyond {} bytes while reading".format(
            MAX_MANIFEST_BYTES
        ),
    )
    manifest = builder.parse_report(content, path)
    require(isinstance(manifest, dict), "candidate manifest must be a JSON object")
    identity = {
        "bytes": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }
    return manifest, content, identity


def first_difference(expected, actual, path="$"):
    if type(expected) is not type(actual):
        return "{} type differs: expected {}, got {}".format(
            path, type(expected).__name__, type(actual).__name__
        )
    if isinstance(expected, dict):
        missing = sorted(set(expected) - set(actual))
        extra = sorted(set(actual) - set(expected))
        if missing:
            return "{} is missing fields: {}".format(path, ", ".join(missing))
        if extra:
            return "{} has unexpected fields: {}".format(path, ", ".join(extra))
        for key in sorted(expected):
            difference = first_difference(
                expected[key], actual[key], "{}.{}".format(path, key)
            )
            if difference is not None:
                return difference
        return None
    if isinstance(expected, list):
        if len(expected) != len(actual):
            return "{} length differs: expected {}, got {}".format(
                path, len(expected), len(actual)
            )
        for index, (expected_item, actual_item) in enumerate(zip(expected, actual)):
            difference = first_difference(
                expected_item, actual_item, "{}[{}]".format(path, index)
            )
            if difference is not None:
                return difference
        return None
    if expected != actual:
        return "{} differs: expected {!r}, got {!r}".format(path, expected, actual)
    return None


def verify_candidate(artifacts_root, version, source_sha):
    require(
        builder.VERSION_RE.fullmatch(version) is not None,
        "version must be stable MAJOR.MINOR.PATCH SemVer",
    )
    require(
        builder.SHA_RE.fullmatch(source_sha) is not None,
        "source SHA must be 40 lowercase hexadecimal characters",
    )
    expected_server_version = "{}+g{}".format(version, source_sha[:12])
    manifest_path = validate_download_layout(artifacts_root)
    manifest, manifest_content, manifest_identity = strict_read_manifest(manifest_path)

    require(
        manifest.get("schema") == builder.MANIFEST_SCHEMA,
        "candidate manifest schema is not {}".format(builder.MANIFEST_SCHEMA),
    )
    require(
        manifest.get("source") == {"git_sha": source_sha},
        "candidate manifest source identity does not match requested SHA",
    )
    require(
        manifest.get("build")
        == {"server_version": expected_server_version, "version": version},
        "candidate manifest build identity does not match requested version/SHA",
    )
    generated_value = manifest.get("generated_at")
    generated_at = builder.validate_timestamp(
        generated_value, "candidate-manifest.generated_at"
    )

    frozen_targets = []
    input_identities = []
    latest_qualification = None
    for target, spec in builder.TARGETS:
        entry, binary_path, binary_identity, report_path, report_identity = (
            builder.validate_target(
                artifacts_root, target, spec, expected_server_version
            )
        )
        frozen_targets.append(entry)
        input_identities.append((binary_path, binary_identity))
        input_identities.append((report_path, report_identity))
        finished_at = builder.validate_timestamp(
            entry["qualification"]["finished_at"],
            "{}.qualification.finished_at".format(target),
        )
        if latest_qualification is None or finished_at > latest_qualification:
            latest_qualification = finished_at

    require(
        latest_qualification is None or generated_at >= latest_qualification,
        "candidate manifest was generated before native qualification finished",
    )
    reconstructed = {
        "build": {
            "server_version": expected_server_version,
            "version": version,
        },
        "generated_at": generated_value,
        "schema": builder.MANIFEST_SCHEMA,
        "source": {"git_sha": source_sha},
        "targets": frozen_targets,
    }
    difference = first_difference(reconstructed, manifest)
    require(
        difference is None,
        "candidate manifest does not match downloaded artifacts: {}".format(
            difference
        ),
    )

    # The approval can be long-lived. Re-inventory, re-read the manifest, and
    # rehash every qualified input after all semantic checks so replacements or
    # concurrent changes cannot pass under identities observed earlier.
    second_manifest_path = validate_download_layout(artifacts_root)
    require(second_manifest_path == manifest_path, "candidate manifest path changed during verification")
    second_manifest, second_content, second_identity = strict_read_manifest(
        second_manifest_path
    )
    require(
        second_identity == manifest_identity
        and second_content == manifest_content
        and second_manifest == manifest,
        "candidate manifest changed during verification",
    )
    for path, identity in input_identities:
        require(
            builder.file_identity(path) == identity,
            "candidate input changed during verification: {}".format(path),
        )

    return {
        "manifest_identity": manifest_identity,
        "server_version": expected_server_version,
        "target_count": len(frozen_targets),
    }


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", help="downloaded Actions artifacts directory")
    parser.add_argument("version", help="requested release version without v prefix")
    parser.add_argument("source_sha", help="full 40-character release source SHA")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    try:
        result = verify_candidate(
            Path(args.artifacts).expanduser(), args.version, args.source_sha
        )
    except (builder.ManifestError, OSError) as error:
        print("error: {}".format(error), file=sys.stderr)
        return 1
    print(
        "verified {} native targets for {} (manifest sha256 {})".format(
            result["target_count"],
            result["server_version"],
            result["manifest_identity"]["sha256"],
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
