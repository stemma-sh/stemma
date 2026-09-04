#!/usr/bin/env python3
"""Build publishable crates from their extracted source archives.

Repository builds can accidentally read files outside a package root. This
check creates each crates.io archive without verification, extracts it into an
unrelated temporary directory, patches internal registry dependencies to the
other extracted archives, and builds/tests only those packaged sources.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parent.parent
PUBLISHED_MANIFESTS = (
    ROOT / "stemma-engine" / "Cargo.toml",
    ROOT / "stemma-diff" / "Cargo.toml",
    ROOT / "stemma-artifacts" / "Cargo.toml",
    ROOT / "stemma-cli" / "Cargo.toml",
)


def run(command: list[str], *, cwd: Path, env: dict[str, str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def package_identity(manifest: Path) -> tuple[str, str]:
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    package = data["package"]
    if package.get("publish") is not True:
        raise RuntimeError(f"expected publish = true in {manifest}")
    return package["name"], package["version"]


def extract_archive(archive: Path, destination: Path, expected_root: str) -> Path:
    with tarfile.open(archive, mode="r:gz") as source:
        members = source.getmembers()
        prefix = expected_root + "/"
        for member in members:
            if member.name != expected_root and not member.name.startswith(prefix):
                raise RuntimeError(
                    f"{archive} contains path outside {expected_root}: {member.name}"
                )
            resolved = (destination / member.name).resolve()
            if not resolved.is_relative_to(destination.resolve()):
                raise RuntimeError(f"{archive} contains unsafe path: {member.name}")
        source.extractall(destination, members=members, filter="data")
    extracted = destination / expected_root
    if not (extracted / "Cargo.toml").is_file():
        raise RuntimeError(f"{archive} did not contain {expected_root}/Cargo.toml")
    return extracted


def main() -> None:
    packages = [
        (manifest, *package_identity(manifest)) for manifest in PUBLISHED_MANIFESTS
    ]

    with tempfile.TemporaryDirectory(prefix="stemma-crate-packages-") as raw_temp:
        scratch = Path(raw_temp)
        package_target = scratch / "package-target"
        extracted_root = scratch / "extracted"
        build_target = scratch / "build-target"
        extracted_root.mkdir()

        package_env = os.environ.copy()
        package_env["CARGO_TARGET_DIR"] = str(package_target)
        extracted: dict[str, Path] = {}

        package_command = [
            "cargo",
            "package",
            "--locked",
            "--allow-dirty",
            "--no-verify",
        ]
        for _manifest, name, _version in packages:
            package_command.extend(("--package", name))
        run(package_command, cwd=ROOT, env=package_env)

        for _manifest, name, version in packages:
            archive = package_target / "package" / f"{name}-{version}.crate"
            if not archive.is_file():
                raise RuntimeError(f"cargo package did not create {archive}")
            extracted[name] = extract_archive(
                archive, extracted_root, f"{name}-{version}"
            )

        workspace_lines = ["[workspace]", "resolver = \"2\"", "members = ["]
        for _manifest, name, version in packages:
            workspace_lines.append(f"  {json.dumps(f'{name}-{version}')},")
        workspace_lines.extend(("]", "", "[patch.crates-io]"))
        for name in ("stemma", "stemma-diff", "stemma-artifacts"):
            workspace_lines.append(
                f"{json.dumps(name)} = {{ path = {json.dumps(str(extracted[name]))} }}"
            )
        isolated_manifest = extracted_root / "Cargo.toml"
        isolated_manifest.write_text(
            "\n".join(workspace_lines) + "\n", encoding="utf-8"
        )

        isolated_env = os.environ.copy()
        isolated_env["CARGO_TARGET_DIR"] = str(build_target)
        run(
            [
                "cargo",
                "build",
                "--workspace",
                "--manifest-path",
                str(isolated_manifest),
            ],
            cwd=extracted_root,
            env=isolated_env,
        )
        run(
            [
                "cargo",
                "test",
                "--workspace",
                "--doc",
                "--manifest-path",
                str(isolated_manifest),
            ],
            cwd=extracted_root,
            env=isolated_env,
        )

    print("crate package check passed: 4 isolated source archives")


if __name__ == "__main__":
    main()
