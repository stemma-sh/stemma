#!/usr/bin/env python3
"""Regenerate and verify the public synthetic redline demo."""

import difflib
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import NoReturn


REPO_ROOT = Path(__file__).resolve().parents[1]
INPUT = REPO_ROOT / "demo" / "before.docx"
WORKLIST = REPO_ROOT / "demo" / "worklist.json"
EXPECTED_REDLINE = REPO_ROOT / "demo" / "expected-redline.docx"
ACCEPTED_TEXT = REPO_ROOT / "demo" / "accepted.txt"
REJECTED_TEXT = REPO_ROOT / "demo" / "rejected.txt"


def fail(message: str) -> NoReturn:
    raise SystemExit(f"demo check failed: {message}")


def resolve_stemma() -> str:
    configured = os.environ.get("STEMMA")
    candidate = Path(configured) if configured else REPO_ROOT / "target/debug/stemma"
    if candidate.is_file():
        return str(candidate)
    found = shutil.which(str(candidate))
    if found:
        return found
    fail(
        f"stemma executable is unavailable: {candidate}\n"
        "build it with: cargo build -p stemma-cli"
    )


def run(stemma: str, *arguments: object) -> str:
    command = [stemma, *(str(argument) for argument in arguments)]
    result = subprocess.run(
        command,
        cwd=REPO_ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        detail = "\n".join(
            part.rstrip() for part in (result.stdout, result.stderr) if part.strip()
        )
        fail(
            f"command exited {result.returncode}: {' '.join(command)}"
            + (f"\n{detail}" if detail else "")
        )
    return result.stdout


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def extract_model(stemma: str, path: Path) -> dict[str, object]:
    try:
        payload = json.loads(run(stemma, "extract", path, "--format", "json"))
        blocks = payload["blocks"]
        revisions = payload["revisions"]
        return {
            "blocks": [
                {"role": block["role"], "text": block["text"]} for block in blocks
            ],
            "revisions": [
                {
                    "kind": revision["kind"],
                    "author": revision["author"],
                    "block_id": revision["block_id"],
                    "excerpt": revision["excerpt"],
                }
                for revision in revisions
            ],
        }
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        fail(f"cannot decode extract model for {path}: {error}")


def require_equal(label: str, expected: object, actual: object) -> None:
    if actual == expected:
        return
    expected_json = json.dumps(expected, indent=2, sort_keys=True).splitlines()
    actual_json = json.dumps(actual, indent=2, sort_keys=True).splitlines()
    difference = "\n".join(
        difflib.unified_diff(
            expected_json,
            actual_json,
            fromfile=f"expected-{label}",
            tofile=f"actual-{label}",
            lineterm="",
        )
    )
    fail(f"{label} differs\n{difference}")


def verify_projection(
    stemma: str,
    temporary: Path,
    label: str,
    redline: Path,
    action: str,
    expected_text: str,
) -> None:
    output = temporary / f"{label}-{action}.docx"
    run(stemma, "resolve", redline, f"--{action}-all", "-o", output)
    actual_text = run(stemma, "extract", output, "--format", "text")
    if actual_text != expected_text:
        difference = "\n".join(
            difflib.unified_diff(
                expected_text.splitlines(),
                actual_text.splitlines(),
                fromfile=f"expected-{action}.txt",
                tofile=f"{label}-{action}.txt",
                lineterm="",
            )
        )
        fail(f"{label} {action}-all projection differs\n{difference}")


def main() -> None:
    stemma = resolve_stemma()
    for required in (
        INPUT,
        WORKLIST,
        EXPECTED_REDLINE,
        ACCEPTED_TEXT,
        REJECTED_TEXT,
    ):
        if not required.is_file():
            fail(f"demo artifact is missing: {required}")

    input_sha = sha256(INPUT)
    accepted_text = ACCEPTED_TEXT.read_text(encoding="utf-8")
    rejected_text = REJECTED_TEXT.read_text(encoding="utf-8")

    with tempfile.TemporaryDirectory(prefix="stemma-demo-check-") as directory:
        temporary = Path(directory)
        generated = temporary / "generated.docx"
        receipt_path = temporary / "receipt.json"

        run(
            stemma,
            "apply",
            INPUT,
            "--worklist",
            WORKLIST,
            "-o",
            generated,
            "--receipt",
            receipt_path,
        )
        if sha256(INPUT) != input_sha:
            fail("demo input changed while generating the redline")

        run(stemma, "validate", generated)
        run(stemma, "validate", EXPECTED_REDLINE)
        require_equal(
            "redline model",
            extract_model(stemma, EXPECTED_REDLINE),
            extract_model(stemma, generated),
        )

        for label, redline in (
            ("generated", generated),
            ("expected", EXPECTED_REDLINE),
        ):
            verify_projection(
                stemma, temporary, label, redline, "accept", accepted_text
            )
            verify_projection(
                stemma, temporary, label, redline, "reject", rejected_text
            )

        try:
            receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            fail(f"cannot decode generated receipt: {error}")
        expected_receipt = {
            "status": "complete",
            "deliverable": True,
            "summary": {"total": 1, "applied": 1, "refused": 0},
        }
        actual_receipt = {
            "status": receipt.get("status"),
            "deliverable": receipt.get("deliverable"),
            "summary": receipt.get("summary"),
        }
        require_equal("receipt outcome", expected_receipt, actual_receipt)

    if sha256(INPUT) != input_sha:
        fail("demo input changed during verification")
    print(
        "demo check passed: generated redline matches the expected revisions; "
        "generated and expected accept/reject projections are exact"
    )


if __name__ == "__main__":
    main()
