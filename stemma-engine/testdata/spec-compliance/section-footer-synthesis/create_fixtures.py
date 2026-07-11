# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for blank footer synthesis tests.

Per ECMA-376 section 17.10.2: when the first section of a document has no
footerReference for the Default kind (and there is no preceding section to
inherit from), a blank/empty default footer should be synthesized — exactly
as is done for headers.

Fixtures are built from raw XML parts assembled into ZIP archives
(no python-docx dependency). This gives precise control over the
exact markup the importer must process.

Run:  uv run create_fixtures.py
"""

import io
import json
import zipfile
from pathlib import Path

ROOT = Path(__file__).parent

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

# -- Shared DOCX skeleton parts -----------------------------------------------

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


def _build_docx(
    document_xml: str,
    extra_parts: dict[str, str | bytes] | None = None,
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
                if isinstance(content, bytes):
                    zf.writestr(path, content)
                else:
                    zf.writestr(path, content)
    return buf.getvalue()


def _save(name: str, docx_bytes: bytes, metadata: dict) -> None:
    out = ROOT / name
    out.mkdir(parents=True, exist_ok=True)
    (out / "input.docx").write_bytes(docx_bytes)
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"  section-footer-synthesis/{name}/")


def _header_xml(text: str) -> str:
    return f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}" xmlns:r="{R}">
  <w:p><w:r><w:t>{text}</w:t></w:r></w:p>
</w:hdr>"""


def _footer_xml(text: str) -> str:
    return f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W}" xmlns:r="{R}">
  <w:p><w:r><w:t>{text}</w:t></w:r></w:p>
</w:ftr>"""


# =========================================================================
# Fixture 1: single-section-no-footer
# =========================================================================

def make_single_section_no_footer() -> None:
    """Single-section document with a default header but NO footer reference.

    ECMA-376 section 17.10.2: If the first (and only) section has no
    footerReference, a blank default footer should be synthesized.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Single section body text. Has header, no footer.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId2"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = (
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>'
    )
    extra_word_rels = (
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>'
    )

    _save(
        "single-section-no-footer",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Default Header"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "single-section-no-footer",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "Single-section document with a default header but no footerReference. "
                "Per spec, a blank default footer should be synthesized."
            ),
        },
    )


# =========================================================================
# Fixture 2: first-section-no-footer
# =========================================================================

def make_first_section_no_footer() -> None:
    """Multi-section document: S1 has no footerReference, S2 has a default footer.

    ECMA-376 section 17.10.2: The first section has no footerReference and
    there is no preceding section to inherit from, so a blank default footer
    should be synthesized.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content - no footer.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content - has a footer.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:footerReference w:type="default" r:id="rId2"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = (
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>'
    )
    extra_word_rels = (
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>'
    )

    _save(
        "first-section-no-footer",
        _build_docx(
            document_xml,
            extra_parts={
                "word/footer1.xml": _footer_xml("Section 2 Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "first-section-no-footer",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "First section has no footerReference. Per spec, a blank footer "
                "should be synthesized. Second section has an explicit footer."
            ),
        },
    )


# =========================================================================
# Fixture 3: single-section-no-footer-no-header
# =========================================================================

def make_single_section_no_footer_no_header() -> None:
    """Single-section document with NO headerReference and NO footerReference.

    ECMA-376 section 17.10.2: Both a blank default header AND a blank
    default footer should be synthesized.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Single section, no header and no footer.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("single-section-no-footer-no-header", _build_docx(document_xml), {
        "name": "single-section-no-footer-no-header",
        "spec_ref": "ECMA-376 section 17.10.2",
        "description": (
            "Single-section document with no headerReference and no footerReference. "
            "Per spec, both a blank header and a blank footer should be synthesized."
        ),
    })


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section Footer Synthesis Fixtures --")
    make_single_section_no_footer()
    make_first_section_no_footer()
    make_single_section_no_footer_no_header()
    print()


if __name__ == "__main__":
    main()
