# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-docx",
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for move tracking and tracked change import fidelity tests.

These fixtures support tests in `stemma-engine/tests/spec_tracked_change_moves.rs`.

Fixtures:
  - reordered-paragraphs/    before+after pair with paragraphs B/C swapped
  - short-paragraphs/        before+after pair with short text (below move threshold)
  - multi-paragraph-move/    before+after pair with multiple paragraphs moved as a group
  - preexisting-insertions/  single DOCX with pre-existing w:ins tracked changes
  - preexisting-deletions/   single DOCX with pre-existing w:del tracked changes

Run:  uv run create_docs.py
"""

import json
from pathlib import Path

from docx import Document
from docx.document import Document as DocxDocument
from docx.oxml.ns import qn
from docx.oxml import OxmlElement

ROOT = Path(__file__).parent

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"


def w(tag: str) -> str:
    return f"{{{W}}}{tag}"


def make_element(tag: str, attribs: dict | None = None) -> OxmlElement:
    el = OxmlElement(tag)
    if attribs:
        for k, v in attribs.items():
            el.set(qn(k), v)
    return el


def save_pair(
    name: str,
    before: DocxDocument,
    after: DocxDocument,
    metadata: dict,
) -> None:
    out = ROOT / name
    out.mkdir(parents=True, exist_ok=True)
    before.save(str(out / "before.docx"))
    after.save(str(out / "after.docx"))
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"  {name}/")


def save_fixture(
    name: str,
    doc: DocxDocument,
    metadata: dict,
    filename: str = "input.docx",
) -> None:
    out = ROOT / name
    out.mkdir(parents=True, exist_ok=True)
    doc.save(str(out / filename))
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"  {name}/")


# =========================================================================
# 1. reordered-paragraphs — paragraphs B/C swapped (text > 20 chars)
# =========================================================================

def make_reordered_paragraphs() -> None:
    """Before: A, B, C. After: A, C, B. Paragraphs have >20 chars for move detection."""
    before = Document()
    before.add_paragraph("This is paragraph Alpha which stays in place at the top of the document.")
    before.add_paragraph("This is paragraph Bravo and it contains enough text for the move detector.")
    before.add_paragraph("This is paragraph Charlie and it also has sufficient text for detection.")

    after = Document()
    after.add_paragraph("This is paragraph Alpha which stays in place at the top of the document.")
    after.add_paragraph("This is paragraph Charlie and it also has sufficient text for detection.")
    after.add_paragraph("This is paragraph Bravo and it contains enough text for the move detector.")

    save_pair("reordered-paragraphs", before, after, {
        "name": "reordered-paragraphs",
        "spec_ref": "ECMA-376 §17.13.5.22-28",
        "description": "Paragraphs B and C swapped; text exceeds 20 char move threshold",
        "expected_behavior": "diff_and_redline should produce moveFrom/moveTo (not del+ins)",
    })


# =========================================================================
# 2. short-paragraphs — text below 20-char move threshold
# =========================================================================

def make_short_paragraphs() -> None:
    """Before: AB, CD. After: CD, AB. Paragraphs have <20 chars — no move detection."""
    before = Document()
    before.add_paragraph("AB")
    before.add_paragraph("CD")

    after = Document()
    after.add_paragraph("CD")
    after.add_paragraph("AB")

    save_pair("short-paragraphs", before, after, {
        "name": "short-paragraphs",
        "spec_ref": "Implementation detail: MOVE_MIN_TEXT_LENGTH = 20",
        "description": "Paragraphs swapped but text is too short for move detection",
        "expected_behavior": "diff_and_redline should produce del+ins (not moveFrom/moveTo)",
    })


# =========================================================================
# 3. multi-paragraph-move — multiple paragraphs moved as a group
# =========================================================================

def make_multi_paragraph_move() -> None:
    """Move the conclusion paragraph from position 5 to position 2.

    Before: Intro, Detail-A, Detail-B, Detail-C, Conclusion
    After:  Intro, Conclusion, Detail-A, Detail-B, Detail-C

    The conclusion paragraph is moved (>20 chars). Detail paragraphs
    each have unique text exceeding the move detection threshold.
    """
    before = Document()
    before.add_paragraph("Introduction paragraph that sets up the document context and remains in place.")
    before.add_paragraph("Detail paragraph Alpha with unique content that exceeds twenty characters easily.")
    before.add_paragraph("Detail paragraph Bravo with distinct content that also exceeds the threshold.")
    before.add_paragraph("Detail paragraph Charlie with separate content that surpasses the limit too.")
    before.add_paragraph("Conclusion paragraph that wraps up the document and will be repositioned.")

    after = Document()
    after.add_paragraph("Introduction paragraph that sets up the document context and remains in place.")
    after.add_paragraph("Conclusion paragraph that wraps up the document and will be repositioned.")
    after.add_paragraph("Detail paragraph Alpha with unique content that exceeds twenty characters easily.")
    after.add_paragraph("Detail paragraph Bravo with distinct content that also exceeds the threshold.")
    after.add_paragraph("Detail paragraph Charlie with separate content that surpasses the limit too.")

    save_pair("multi-paragraph-move", before, after, {
        "name": "multi-paragraph-move",
        "spec_ref": "ECMA-376 §17.13.5.22-28",
        "description": "Multiple paragraphs moved as a group; each paragraph exceeds 20-char threshold",
        "expected_behavior": "All moved paragraphs should have matching moveFrom/moveTo range markers linked by name",
    })


# =========================================================================
# 4. preexisting-insertions — DOCX with pre-existing w:ins
# =========================================================================

def make_preexisting_insertions() -> None:
    """A DOCX with pre-existing w:ins around some text.

    Paragraph 1: "Hello " + [inserted: "wonderful "] + "world."
    The w:ins wrapper has author="Insert Author" and date="2025-03-01T12:00:00Z".
    """
    doc = Document()
    p = doc.add_paragraph()

    # Normal run: "Hello "
    p.add_run("Hello ")

    # Inserted run: "wonderful "
    ins_wrapper = make_element("w:ins", {
        "w:id": "100",
        "w:author": "Insert Author",
        "w:date": "2025-03-01T12:00:00Z",
    })
    run_ins = OxmlElement("w:r")
    t = make_element("w:t")
    t.text = "wonderful "
    t.set(qn("xml:space"), "preserve")
    run_ins.append(t)
    ins_wrapper.append(run_ins)
    p._p.append(ins_wrapper)

    # Normal run: "world."
    p.add_run("world.")

    save_fixture("preexisting-insertions", doc, {
        "name": "preexisting-insertions",
        "spec_ref": "ISO 29500-1 §17.13.5.18",
        "description": "Pre-existing w:ins with author/date around 'wonderful '",
        "expected_behavior": "import_docx().canonical should show TrackedSegment with Inserted status and RevisionInfo",
    })


# =========================================================================
# 5. preexisting-deletions — DOCX with pre-existing w:del
# =========================================================================

def make_preexisting_deletions() -> None:
    """A DOCX with pre-existing w:del around some text.

    Paragraph 1: "The " + [deleted: "original "] + "revised document."
    The w:del wrapper has author="Delete Author" and date="2025-04-15T09:30:00Z".
    """
    doc = Document()
    p = doc.add_paragraph()

    # Normal run: "The "
    p.add_run("The ")

    # Deleted run: "original "
    del_wrapper = make_element("w:del", {
        "w:id": "200",
        "w:author": "Delete Author",
        "w:date": "2025-04-15T09:30:00Z",
    })
    run_del = OxmlElement("w:r")
    del_t = make_element("w:delText")
    del_t.text = "original "
    del_t.set(qn("xml:space"), "preserve")
    run_del.append(del_t)
    del_wrapper.append(run_del)
    p._p.append(del_wrapper)

    # Normal run: "revised document."
    p.add_run("revised document.")

    save_fixture("preexisting-deletions", doc, {
        "name": "preexisting-deletions",
        "spec_ref": "ISO 29500-1 §17.13.5.4",
        "description": "Pre-existing w:del with w:delText and author/date around 'original '",
        "expected_behavior": "import_docx().canonical should show TrackedSegment with Deleted status and RevisionInfo",
    })


# =========================================================================
# 6. preexisting-moves — DOCX with pre-existing w:moveFrom/w:moveTo
# =========================================================================

def make_preexisting_moves() -> None:
    """A Word-valid DOCX with a pre-existing move of a whole paragraph.

    Paragraph layout:
      P1: "Paragraph Alpha stays in place."
      P2: moved-to "Paragraph Charlie was moved here."   (range name "move_0")
      P3: "Paragraph Bravo stays in place."
      P4: moved-from "Paragraph Charlie was moved here." (range name "move_0")
      P5: "Paragraph Delta closes the document."

    Written in Word's own shape: each moved paragraph's mark carries
    w:moveTo/w:moveFrom, its runs sit in a w:moveTo/w:moveFrom container
    holding plain w:t, the range start sits inside the paragraph and the range
    end follows it at body level, and w:sectPr stays the body's last child.
    """
    from lxml import etree

    doc = Document()
    body = doc.element.body
    for p in body.findall(qn("w:p")):
        body.remove(p)
    sect_pr = body.find(qn("w:sectPr"))

    m = 'w:author="Move Author" w:date="2025-06-01T12:00:00Z"'
    xml = (
        '<w:p><w:r><w:t>Paragraph Alpha stays in place.</w:t></w:r></w:p>'
        f'<w:p><w:pPr><w:rPr><w:moveTo w:id="20" {m}/></w:rPr></w:pPr>'
        f'<w:moveToRangeStart w:id="10" w:name="move_0" {m}/>'
        f'<w:moveTo w:id="200" {m}><w:r><w:t>Paragraph Charlie was moved here.</w:t></w:r></w:moveTo></w:p>'
        '<w:moveToRangeEnd w:id="10"/>'
        '<w:p><w:r><w:t>Paragraph Bravo stays in place.</w:t></w:r></w:p>'
        f'<w:p><w:pPr><w:rPr><w:moveFrom w:id="21" {m}/></w:rPr></w:pPr>'
        f'<w:moveFromRangeStart w:id="11" w:name="move_0" {m}/>'
        f'<w:moveFrom w:id="201" {m}><w:r><w:t>Paragraph Charlie was moved here.</w:t></w:r></w:moveFrom></w:p>'
        '<w:moveFromRangeEnd w:id="11"/>'
        '<w:p><w:r><w:t>Paragraph Delta closes the document.</w:t></w:r></w:p>'
    )
    fragment = etree.fromstring(f'<w:root xmlns:w="{W}">{xml}</w:root>')
    for element in list(fragment):
        sect_pr.addprevious(element)

    save_fixture("preexisting-moves", doc, {
        "name": "preexisting-moves",
        "spec_ref": "ECMA-376 \u00a717.13.5.21-28",
        "description": "Pre-existing whole-paragraph move in Word's shape, ranges linked by name='move_0'",
        "expected_behavior": "import_docx() reads one move: the two paragraphs share move_id 'move_0', one moved-from and one moved-to",
    })


# =========================================================================
# Main
# =========================================================================

if __name__ == "__main__":
    print("\n── Tracked Change Moves ──")
    make_reordered_paragraphs()
    make_short_paragraphs()
    make_multi_paragraph_move()
    make_preexisting_insertions()
    make_preexisting_deletions()
    make_preexisting_moves()
    print("\nDone.")
