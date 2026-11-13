"""The Python binding's contract, exercised through the installed package.

Expected values come from the document domain (accept-all yields the edited
text, reject-all the original; a tracked edit is a census row and not a direct
change), never from what the binding happens to return.
"""

from __future__ import annotations

import ast
import hashlib
import importlib.metadata
import io
import json
import os
import zipfile
from pathlib import Path

import pytest

import stemma

AUTHOR = "J. Osei"
NEW_TEXT = "Liability is capped at twice the fees paid."


def make_docx(*paragraphs: str) -> bytes:
    """A minimal valid WordprocessingML package, one run per paragraph."""
    body = "".join(f"<w:p><w:r><w:t>{text}</w:t></w:r></w:p>" for text in paragraphs)
    document = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
        f"<w:body>{body}<w:sectPr/></w:body></w:document>"
    )
    content_types = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
        "</Types>"
    )
    root_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>'
        "</Relationships>"
    )
    document_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>'
    )
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as package:
        package.writestr("[Content_Types].xml", content_types)
        package.writestr("_rels/.rels", root_rels)
        package.writestr("word/_rels/document.xml.rels", document_rels)
        package.writestr("word/document.xml", document)
    return buffer.getvalue()


def replace_txn(block: dict, text: str, author: str = AUTHOR, **extra) -> dict:
    return {
        "ops": [
            {
                "op": "replace",
                "target": block["id"],
                "guard": block["guard"],
                "content": {"type": "paragraph", "content": [{"type": "text", "text": text}]},
            }
        ],
        "revision": {"author": author, "date": "2026-07-06T10:30:00Z"},
        **extra,
    }


@pytest.fixture
def source(tmp_path: Path) -> Path:
    path = tmp_path / "contract.docx"
    path.write_bytes(make_docx("Definitions.", "Liability is capped at the fees paid."))
    return path


# -- the one-edit cycle -----------------------------------------------------


def test_one_edit_cycle_from_path_to_saved_redline(source: Path, tmp_path: Path) -> None:
    doc = stemma.Document.open(source)
    assert doc.diagnostics() == []

    blocks = doc.read()["blocks"]
    assert [b["text"] for b in blocks] == ["Definitions.", "Liability is capped at the fees paid."]

    edited = doc.apply_authored(replace_txn(blocks[1], NEW_TEXT))

    # One file, two readings: accept-all is the edit, reject-all the original.
    assert edited.read_accepted().to_text() == f"Definitions.\n\n{NEW_TEXT}"
    assert edited.read_rejected().to_text() == "Definitions.\n\nLiability is capped at the fees paid."
    # Immutability: the source value still reads as it was opened.
    assert doc.to_text() == "Definitions.\n\nLiability is capped at the fees paid."
    assert doc.revisions() == []

    revisions = edited.revisions()
    assert revisions, "a tracked edit is pending revisions"
    assert {r["author"] for r in revisions} == {AUTHOR}
    assert all(r["block_id"] == blocks[1]["id"] for r in revisions)
    assert all(r["revision_id"] != 0 for r in revisions)

    # The edit is tracked, so review finds census rows, no direct change, and
    # the untouched first paragraph proven unchanged.
    review = edited.review()
    assert review["new_revisions"]
    assert review["direct_changes"] == []
    assert review["untouched"]["violations"] == []
    assert review["untouched"]["verified_blocks"] >= 1
    assert review["validator"] == {"ok": True, "issues": []}

    data = edited.serialize()
    assert stemma.validate(data) == {"ok": True, "issues": []}

    out = tmp_path / "contract-redline.docx"
    receipt = edited.save(out)
    written = out.read_bytes()
    assert set(receipt) == {"path", "bytes", "sha256"}
    # The resolved destination (on Windows a canonical path may carry the
    # verbatim prefix, so compare file identity rather than spelling).
    assert os.path.samefile(receipt["path"], out)
    assert receipt["bytes"] == len(written)
    assert receipt["sha256"] == hashlib.sha256(written).hexdigest()
    reopened = stemma.Document.open(out)
    assert reopened.read_accepted().to_text() == edited.read_accepted().to_text()
    assert reopened.read_rejected().to_text() == edited.read_rejected().to_text()


def test_from_bytes_and_a_json_string_transaction_match_the_dict_form(source: Path) -> None:
    doc = stemma.Document.from_bytes(source.read_bytes())
    block = doc.read()["blocks"][1]
    txn = replace_txn(block, NEW_TEXT)
    from_dict = doc.apply(txn)
    from_text = doc.apply(json.dumps(txn))
    assert from_dict.read_accepted().to_text() == from_text.read_accepted().to_text()
    assert from_dict.to_markdown() == from_text.to_markdown()


def test_check_is_the_dry_run_of_apply(source: Path) -> None:
    doc = stemma.Document.open(source)
    block = doc.read()["blocks"][1]
    assert doc.check(replace_txn(block, NEW_TEXT)) is None
    changed = doc.apply(replace_txn(block, "Liability is unlimited.", materialization_mode="direct"))
    with pytest.raises(stemma.StaleEdit):
        changed.check(replace_txn(block, NEW_TEXT))


def test_direct_mode_is_reported_as_a_direct_change(source: Path) -> None:
    doc = stemma.Document.open(source)
    block = doc.read()["blocks"][1]
    edited = doc.apply(replace_txn(block, NEW_TEXT, materialization_mode="direct"))
    assert edited.revisions() == []
    assert edited.to_text() == f"Definitions.\n\n{NEW_TEXT}"
    review = edited.review()
    assert review["new_revisions"] == []
    assert [c["kind"] for c in review["direct_changes"]] == ["block_modified"]


# -- save ----------------------------------------------------------------


def test_save_refuses_an_existing_path_unless_overwrite(source: Path, tmp_path: Path) -> None:
    doc = stemma.Document.open(source)
    edited = doc.apply(replace_txn(doc.read()["blocks"][1], NEW_TEXT))
    out = tmp_path / "out.docx"
    out.write_bytes(b"keep me")

    with pytest.raises(FileExistsError):
        edited.save(out)
    assert out.read_bytes() == b"keep me"

    # The opened source is protected the same way.
    with pytest.raises(FileExistsError):
        edited.save(source)

    receipt = edited.save(out, overwrite=True)
    written = out.read_bytes()
    assert receipt["sha256"] == hashlib.sha256(written).hexdigest()
    assert stemma.Document.open(out).read_accepted().to_text() == edited.read_accepted().to_text()


@pytest.mark.skipif(os.name != "posix", reason="POSIX file modes")
def test_save_overwrite_keeps_the_replaced_files_mode(source: Path, tmp_path: Path) -> None:
    # Overwriting replaces the content, not the file's access policy: a
    # group-writable document stays group-writable.
    doc = stemma.Document.open(source)
    out = tmp_path / "shared.docx"
    out.write_bytes(b"old")
    out.chmod(0o664)
    doc.save(out, overwrite=True)
    assert out.stat().st_mode & 0o777 == 0o664


@pytest.mark.skipif(os.name != "posix", reason="POSIX file modes")
def test_save_overwrite_creates_a_new_file_with_the_umask_mode(source: Path, tmp_path: Path) -> None:
    # A file that did not exist is created like any other new file: 0666
    # less the process umask, never a private 0600 staging mode.
    doc = stemma.Document.open(source)
    umask = os.umask(0o022)
    try:
        out = tmp_path / "fresh.docx"
        doc.save(out, overwrite=True)
    finally:
        os.umask(umask)
    assert out.stat().st_mode & 0o777 == 0o644


def test_open_missing_file_raises_file_not_found(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        stemma.Document.open(tmp_path / "absent.docx")


# -- refusals ------------------------------------------------------------


def test_stale_guard_raises_stale_edit_with_structured_details(source: Path) -> None:
    doc = stemma.Document.open(source)
    planned = doc.read()["blocks"][1]
    # Someone else changes the block after the plan was read.
    changed = doc.apply(replace_txn(planned, "Liability is unlimited.", materialization_mode="direct"))
    current = changed.read()["blocks"][1]

    with pytest.raises(stemma.StaleEdit) as caught:
        changed.apply(replace_txn(planned, NEW_TEXT))

    error = caught.value
    assert isinstance(error, stemma.StemmaError)
    assert error.code == "StaleEdit"
    assert error.message
    assert str(error).startswith("StaleEdit: ")
    assert error.details["block_id"] == planned["id"]
    assert error.details["step_index"] == 0
    # The refusal carries both guards, so the caller can re-plan against the
    # current one without another read.
    detail = error.details["stale_edit"]
    assert detail["kind"] == "semantic_hash_mismatch"
    assert detail["target_block_id"] == planned["id"]
    assert detail["expected"] == planned["guard"]
    assert detail["actual"] == current["guard"]
    # Atomic: the refused document is unchanged.
    assert changed.revisions() == []
    assert changed.to_text() == "Definitions.\n\nLiability is unlimited."


def test_reused_origin_author_requires_confirmation(source: Path) -> None:
    seeded = stemma.Document.open(source)
    seeded = seeded.apply(replace_txn(seeded.read()["blocks"][1], NEW_TEXT, author="Counsel"))
    doc = stemma.Document.from_bytes(seeded.serialize())
    first = doc.read()["blocks"][0]

    with pytest.raises(stemma.AuthorLabelCollision) as caught:
        doc.apply_authored(replace_txn(first, "Definitions and terms.", author="Counsel"))
    assert caught.value.code == "AuthorLabelCollision"
    collision = caught.value.details["author_label_collision"]
    assert collision["author_label"] == "Counsel"
    assert collision["existing_revision_count"] > 0

    continued = doc.apply_authored(
        replace_txn(first, "Definitions and terms.", author="Counsel"),
        allow_existing_author=True,
    )
    assert "Definitions and terms." in continued.read_accepted().to_text()


@pytest.mark.parametrize(
    "transaction, code",
    [
        ("{not json", "schema_error"),
        ({"ops": [], "revision": {"author": AUTHOR}}, "schema_error"),
        ({"ops": [{"op": "replace", "nonsense": 1}], "revision": {"author": AUTHOR}}, "schema_error"),
    ],
)
def test_malformed_transactions_raise_transaction_error(
    source: Path, transaction: object, code: str
) -> None:
    doc = stemma.Document.open(source)
    with pytest.raises(stemma.TransactionError) as caught:
        doc.apply(transaction)
    assert caught.value.code == code
    assert caught.value.details == {}


def test_a_transaction_must_be_a_string_or_a_dict(source: Path) -> None:
    doc = stemma.Document.open(source)
    with pytest.raises(TypeError):
        doc.apply([{"op": "replace"}])
    with pytest.raises(ValueError):
        doc.apply({"ops": [], "revision": {"author": AUTHOR}, "x": float("nan")})


def test_non_docx_bytes_are_refused_by_parse_and_reported_by_validate() -> None:
    with pytest.raises(stemma.StemmaError) as caught:
        stemma.Document.from_bytes(b"not a zip archive")
    assert caught.value.code == "InvalidDocx"

    report = stemma.validate(b"not a zip archive")
    assert report["ok"] is False
    assert report["issues"]


# -- resolution ------------------------------------------------------------


def test_selective_projection_resolves_only_the_selected_revisions(source: Path) -> None:
    doc = stemma.Document.open(source)
    blocks = doc.read()["blocks"]
    edited = doc.apply(replace_txn(blocks[0], "Terms.", author="First"))
    edited = edited.apply(replace_txn(edited.read()["blocks"][1], NEW_TEXT, author="Second"))

    first_ids = {r["revision_id"] for r in edited.revisions() if r["author"] == "First"}
    accepted_first = edited.project("accept", ids=first_ids)
    assert {r["author"] for r in accepted_first.revisions()} == {"Second"}
    assert accepted_first.read_rejected().to_text() == "Terms.\n\nLiability is capped at the fees paid."

    rest = [r["revision_id"] for r in accepted_first.revisions()]
    resolved = accepted_first.project("reject", ids=rest)
    assert resolved.revisions() == []
    assert resolved.to_text() == "Terms.\n\nLiability is capped at the fees paid."

    assert edited.project("accept_all").to_text() == edited.read_accepted().to_text()
    assert edited.project("reject_all").to_text() == edited.read_rejected().to_text()


def test_projection_arguments_are_validated(source: Path) -> None:
    doc = stemma.Document.open(source)
    with pytest.raises(stemma.InvalidRange) as caught:
        doc.project("accept", ids=[])
    assert caught.value.code == "InvalidRange"
    with pytest.raises(ValueError):
        doc.project("accept")
    with pytest.raises(ValueError):
        doc.project("accept_all", ids=[1])
    with pytest.raises(ValueError):
        doc.project("accept_some", ids=[1])
    with pytest.raises(OverflowError):
        doc.project("accept", ids=[-1])
    with pytest.raises(TypeError):
        doc.project("accept", ids="12")


# -- compare / audit ---------------------------------------------------------


def test_compare_produces_a_redline_between_the_two_readings() -> None:
    base = stemma.Document.from_bytes(make_docx("Definitions.", "Liability is capped at the fees paid."))
    target = stemma.Document.from_bytes(make_docx("Definitions.", NEW_TEXT))

    result = stemma.compare(base, target, author="Reviewer")
    assert isinstance(result, stemma.Comparison)
    assert result.semantic_change_count >= 1
    assert result.base_diagnostics == []
    assert result.target_diagnostics == []
    assert result.flattened_input_revisions == {"base": [], "target": []}

    redline = result.document
    assert redline.read_rejected().to_text() == base.to_text()
    assert redline.read_accepted().to_text() == target.to_text()
    assert {r["author"] for r in redline.revisions()} == {"Reviewer"}

    anonymous = stemma.compare(base, target).document
    assert anonymous.read_accepted().to_text() == target.to_text()
    # Anonymous: the revisions carry no author label at all.
    assert {r["author"] for r in anonymous.revisions()} == {None}

    with pytest.raises(stemma.StemmaError) as caught:
        stemma.compare(base, target, author="")
    assert caught.value.code == "ValidationFailed"


def test_audit_certifies_a_tracked_delivery_and_flags_an_untracked_one(source: Path) -> None:
    before = source.read_bytes()
    doc = stemma.Document.from_bytes(before)
    block = doc.read()["blocks"][1]

    tracked = doc.apply(replace_txn(block, NEW_TEXT)).serialize()
    report = stemma.audit(before, tracked)
    assert report["new_revisions"]
    assert {r["author"] for r in report["new_revisions"]} == {AUTHOR}
    assert report["direct_changes"] == []
    assert report["preexisting_revisions"] == []
    assert report["untouched"]["violations"] == []
    assert report["validator"]["ok"] is True

    direct = doc.apply(replace_txn(block, NEW_TEXT, materialization_mode="direct")).serialize()
    report = stemma.audit(before, direct)
    assert report["new_revisions"] == []
    assert [c["kind"] for c in report["direct_changes"]] == ["block_modified"]


# -- packaging -------------------------------------------------------------


def test_the_distribution_has_no_runtime_dependencies() -> None:
    assert not importlib.metadata.requires("stemma-py")
    assert importlib.metadata.version("stemma-py") == stemma.__version__


def test_the_documented_shapes_are_importable_at_runtime() -> None:
    # Annotations such as ``def f(r: stemma.Revision)`` must not fail when
    # evaluated at runtime (dataclasses, pydantic, typing.get_type_hints).
    from stemma import Revision, SaveReceipt, Transaction, ValidationReport

    assert "kind" in Revision.__annotations__
    assert set(SaveReceipt.__annotations__) == {"path", "bytes", "sha256"}
    assert "issues" in ValidationReport.__annotations__
    assert Transaction is not None


def test_bytes_like_inputs_are_accepted(source: Path) -> None:
    data = source.read_bytes()
    for wrap in (bytes, bytearray, memoryview):
        doc = stemma.Document.from_bytes(wrap(data))
        assert doc.to_text() == stemma.Document.from_bytes(data).to_text()
        assert stemma.validate(wrap(data))["ok"] is True
        assert stemma.audit(wrap(data), wrap(data))["direct_changes"] == []
    with pytest.raises(TypeError, match="data must be bytes, bytearray or memoryview"):
        stemma.Document.from_bytes("not bytes")


def test_the_stub_declares_exactly_the_runtime_surface() -> None:
    stub_path = Path(stemma._native.__file__).with_name("_native.pyi")
    tree = ast.parse(stub_path.read_text(encoding="utf-8"))
    stub_names: dict[str, set[str]] = {}
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            stub_names[node.name] = {
                item.name for item in node.body if isinstance(item, ast.FunctionDef)
            }
        elif isinstance(node, ast.FunctionDef):
            stub_names[node.name] = set()
    runtime = {name for name in dir(stemma._native) if not name.startswith("_")}
    assert set(stub_names) == runtime, "stub and native module declare different names"
    shapes = {"Transaction", "Diagnostic", "Revision", "ValidationIssue", "ValidationReport",
              "SaveReceipt", "FlattenedInputRevisions"}
    assert set(stemma.__all__) - {"__version__"} == runtime | shapes
    for name in ("Document", "Comparison"):
        members = {m for m in vars(getattr(stemma, name)) if not m.startswith("_")}
        assert stub_names[name] == members, f"stub and runtime differ on {name}"
