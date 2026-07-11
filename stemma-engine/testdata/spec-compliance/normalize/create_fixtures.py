# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Generate DOCX fixtures for normalization testing.

Each fixture exercises a specific tracked-change or comment construct that the
normalization pipeline must handle. Tests in `stemma-engine/tests/normalize.rs`
import these fixtures and assert the normalized output.

Fixtures are built from raw XML parts assembled into ZIP archives — no
python-docx dependency required. This gives precise control over the exact
markup the normalizer must process.

Run:  uv run create_fixtures.py
"""

import io
import json
import zipfile
from pathlib import Path

ROOT = Path(__file__).parent

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
MC = "http://schemas.openxmlformats.org/markup-compatibility/2006"

# ── Shared DOCX skeleton parts ────────────────────────────────────────────

CONTENT_TYPES_BASE = """\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml"
    ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
{extra_overrides}
</Types>
"""

TOP_RELS = """\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>
"""

WORD_RELS_BASE = """\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
{extra_rels}
</Relationships>
"""


def _wrap_body(body_xml: str) -> str:
    """Wrap body XML in a full w:document envelope."""
    return f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}" xmlns:mc="{MC}">
  <w:body>
{body_xml}
    <w:sectPr/>
  </w:body>
</w:document>
"""


def _build_docx(
    document_xml: str,
    extra_parts: dict[str, str] | None = None,
    extra_overrides: str = "",
    extra_word_rels: str = "",
) -> bytes:
    """Assemble a minimal DOCX ZIP from raw XML strings."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(
            "[Content_Types].xml",
            CONTENT_TYPES_BASE.format(extra_overrides=extra_overrides),
        )
        zf.writestr("_rels/.rels", TOP_RELS)
        zf.writestr(
            "word/_rels/document.xml.rels",
            WORD_RELS_BASE.format(extra_rels=extra_word_rels),
        )
        zf.writestr("word/document.xml", document_xml)
        if extra_parts:
            for path, content in extra_parts.items():
                zf.writestr(path, content)
    return buf.getvalue()


def _save(name: str, docx_bytes: bytes, metadata: dict) -> None:
    out = ROOT / name
    out.mkdir(parents=True, exist_ok=True)
    (out / "input.docx").write_bytes(docx_bytes)
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"  normalize/{name}/")


# =========================================================================
# Fixture: main-ins-del
# =========================================================================

def make_main_ins_del() -> None:
    """Main document body with w:ins and w:del tracked changes."""
    body = f"""\
    <w:p>
      <w:r><w:t xml:space="preserve">This is </w:t></w:r>
      <w:ins w:id="1" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t xml:space="preserve">inserted </w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve">text with </w:t></w:r>
      <w:del w:id="2" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText xml:space="preserve">deleted </w:delText></w:r>
      </w:del>
      <w:r><w:t>content.</w:t></w:r>
    </w:p>"""

    docx = _build_docx(_wrap_body(body))
    _save("main-ins-del", docx, {
        "name": "main-ins-del",
        "description": "Main body with w:ins and w:del tracked changes",
        "revision_elements": {"ins": 1, "del": 1, "delText": 1},
        "expected_after_normalize": "This is inserted text with content.",
    })


# =========================================================================
# Fixture: main-moves
# =========================================================================

def make_main_moves() -> None:
    """Main document body with w:moveFrom and w:moveTo tracked changes."""
    body = f"""\
    <w:p>
      <w:r><w:t xml:space="preserve">Start </w:t></w:r>
      <w:moveFrom w:id="10" w:author="TestUser" w:date="2024-01-01T00:00:00Z" w:name="move1">
        <w:r><w:t xml:space="preserve">moved text </w:t></w:r>
      </w:moveFrom>
      <w:r><w:t xml:space="preserve">middle </w:t></w:r>
      <w:moveTo w:id="11" w:author="TestUser" w:date="2024-01-01T00:00:00Z" w:name="move1">
        <w:r><w:t xml:space="preserve">moved text </w:t></w:r>
      </w:moveTo>
      <w:r><w:t>end.</w:t></w:r>
    </w:p>"""

    docx = _build_docx(_wrap_body(body))
    _save("main-moves", docx, {
        "name": "main-moves",
        "description": "Main body with w:moveFrom and w:moveTo tracked changes",
        "revision_elements": {"moveFrom": 1, "moveTo": 1},
        "expected_after_normalize": "Start middle moved text end.",
    })


# =========================================================================
# Fixture: main-format-changes
# =========================================================================

def make_main_format_changes() -> None:
    """Main document body with w:rPrChange and w:pPrChange formatting changes."""
    body = f"""\
    <w:p>
      <w:pPr>
        <w:jc w:val="center"/>
        <w:pPrChange w:id="3" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
          <w:pPr><w:jc w:val="left"/></w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r>
        <w:rPr>
          <w:b/>
          <w:rPrChange w:id="4" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
            <w:rPr/>
          </w:rPrChange>
        </w:rPr>
        <w:t>Bold centered text</w:t>
      </w:r>
    </w:p>"""

    docx = _build_docx(_wrap_body(body))
    _save("main-format-changes", docx, {
        "name": "main-format-changes",
        "description": "Main body with w:pPrChange and w:rPrChange formatting tracked changes",
        "revision_elements": {"pPrChange": 1, "rPrChange": 1},
        "expected_after_normalize": "pPrChange and rPrChange removed; w:b and w:jc preserved",
    })


# =========================================================================
# Fixture: header-tracked-changes
# =========================================================================

def make_header_tracked_changes() -> None:
    """Header part containing w:ins and w:del tracked changes."""
    header_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}" xmlns:r="{R}">
  <w:p>
    <w:r><w:t xml:space="preserve">Header </w:t></w:r>
    <w:ins w:id="20" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
      <w:r><w:t xml:space="preserve">new </w:t></w:r>
    </w:ins>
    <w:del w:id="21" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
      <w:r><w:delText xml:space="preserve">old </w:delText></w:r>
    </w:del>
    <w:r><w:t>text</w:t></w:r>
  </w:p>
</w:hdr>
"""

    # Main body is clean — no revisions
    body = """\
    <w:p>
      <w:r><w:t>Clean body content.</w:t></w:r>
    </w:p>"""

    # Reference the header from sectPr
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}" xmlns:mc="{MC}">
  <w:body>
{body}
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId1"/>
    </w:sectPr>
  </w:body>
</w:document>
"""

    extra_overrides = '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>'
    extra_word_rels = '  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>'

    docx = _build_docx(
        document_xml,
        extra_parts={"word/header1.xml": header_xml},
        extra_overrides=extra_overrides,
        extra_word_rels=extra_word_rels,
    )
    _save("header-tracked-changes", docx, {
        "name": "header-tracked-changes",
        "description": "Header part with w:ins and w:del tracked changes; main body is clean",
        "revision_elements": {"ins": 1, "del": 1, "delText": 1},
        "revision_parts": ["word/header1.xml"],
        "expected_after_normalize": "Header text becomes 'Header new text'",
    })


# =========================================================================
# Fixture: comments-basic
# =========================================================================

def make_comments_basic() -> None:
    """Document with comment ranges that should be preserved after normalization."""
    body = f"""\
    <w:p>
      <w:r><w:t xml:space="preserve">Before </w:t></w:r>
      <w:commentRangeStart w:id="30"/>
      <w:r><w:t xml:space="preserve">commented text</w:t></w:r>
      <w:commentRangeEnd w:id="30"/>
      <w:r>
        <w:rPr><w:rStyle w:val="CommentReference"/></w:rPr>
        <w:commentReference w:id="30"/>
      </w:r>
      <w:r><w:t xml:space="preserve"> after.</w:t></w:r>
    </w:p>"""

    comments_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{W}" xmlns:r="{R}">
  <w:comment w:id="30" w:author="Reviewer" w:date="2024-01-01T00:00:00Z" w:initials="R">
    <w:p>
      <w:r><w:t>This is a comment.</w:t></w:r>
    </w:p>
  </w:comment>
</w:comments>
"""

    extra_overrides = '  <Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>'
    extra_word_rels = '  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>'

    docx = _build_docx(
        _wrap_body(body),
        extra_parts={"word/comments.xml": comments_xml},
        extra_overrides=extra_overrides,
        extra_word_rels=extra_word_rels,
    )
    _save("comments-basic", docx, {
        "name": "comments-basic",
        "description": "Document with comment range markers and comments.xml; should be preserved",
        "comment_ranges": 1,
        "expected_after_normalize": "Comment ranges and comments.xml preserved unchanged",
    })


# =========================================================================
# Fixture: sdt-with-revisions
# =========================================================================

def make_sdt_with_revisions() -> None:
    """Content control (w:sdt) containing tracked changes inside."""
    body = f"""\
    <w:sdt>
      <w:sdtPr>
        <w:alias w:val="TestControl"/>
      </w:sdtPr>
      <w:sdtContent>
        <w:p>
          <w:r><w:t xml:space="preserve">Inside control </w:t></w:r>
          <w:ins w:id="40" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
            <w:r><w:t xml:space="preserve">added </w:t></w:r>
          </w:ins>
          <w:del w:id="41" w:author="TestUser" w:date="2024-01-01T00:00:00Z">
            <w:r><w:delText xml:space="preserve">removed </w:delText></w:r>
          </w:del>
          <w:r><w:t>text.</w:t></w:r>
        </w:p>
      </w:sdtContent>
    </w:sdt>
    <w:p>
      <w:r><w:t>After the content control.</w:t></w:r>
    </w:p>"""

    docx = _build_docx(_wrap_body(body))
    _save("sdt-with-revisions", docx, {
        "name": "sdt-with-revisions",
        "description": "Content control (w:sdt) with tracked changes inside sdtContent",
        "revision_elements": {"ins": 1, "del": 1, "delText": 1},
        "expected_after_normalize": "Inside control added text. (w:sdt structure preserved)",
    })


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n── Normalization Fixtures ──")
    make_main_ins_del()
    make_main_moves()
    make_main_format_changes()
    make_header_tracked_changes()
    make_comments_basic()
    make_sdt_with_revisions()
    print()


if __name__ == "__main__":
    main()
