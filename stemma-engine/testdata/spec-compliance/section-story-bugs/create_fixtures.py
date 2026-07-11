# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for section/story behavioral bug tests.

Each fixture exercises a specific section-properties or header/footer
handling scenario per ECMA-376 / ISO 29500-1.

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
    print(f"  section-story-bugs/{name}/")


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
# Fixture: next-column-section-type
# =========================================================================

def make_next_column_section_type() -> None:
    """Document with a nextColumn section break.

    ECMA-376 section 17.6.22 (ST_SectionMark): w:type w:val="nextColumn" is a valid
    section type. The importer must parse it into SectionType::NextColumn.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:type w:val="nextColumn"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
          <w:cols w:num="2" w:space="720"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (next column).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
      <w:cols w:num="2" w:space="720"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("next-column-section-type", _build_docx(document_xml), {
        "name": "next-column-section-type",
        "spec_ref": "ECMA-376 section 17.6.22 (ST_SectionMark)",
        "description": "Section 1 has w:type w:val='nextColumn'. Must parse as SectionType::NextColumn.",
    })


# =========================================================================
# Fixture: title-page-false-with-first-header
# =========================================================================

def make_title_page_false_with_first_header() -> None:
    """Document with titlePg absent and both default + first-page headers.

    ECMA-376 section 17.10.6: When titlePg is absent or false, the first-page
    header/footer SHALL NOT be shown. The model should either filter out
    first-page headers/footers or mark them as inactive.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Body content with no titlePg flag.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId2"/>
      <w:headerReference w:type="first" r:id="rId3"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = (
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>\n'
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>'
    )
    extra_word_rels = (
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>\n'
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>'
    )

    _save(
        "title-page-false-with-first-header",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Default Header Text"),
                "word/header2.xml": _header_xml("First Page Header Text"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "title-page-false-with-first-header",
            "spec_ref": "ECMA-376 section 17.10.6",
            "description": (
                "titlePg absent + both default and first-page headers present. "
                "Per spec, first-page header should not be active."
            ),
        },
    )


# =========================================================================
# Fixture: title-page-explicit-false-with-first-header
# =========================================================================

def make_title_page_explicit_false_with_first_header() -> None:
    """Document with w:titlePg w:val="false" and both default + first-page headers.

    Same as above but with an explicit false value on the element.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Body with titlePg explicitly false.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId2"/>
      <w:headerReference w:type="first" r:id="rId3"/>
      <w:titlePg w:val="false"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = (
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>\n'
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>'
    )
    extra_word_rels = (
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>\n'
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>'
    )

    _save(
        "title-page-explicit-false-with-first-header",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Default Header"),
                "word/header2.xml": _header_xml("First Page Header"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "title-page-explicit-false-with-first-header",
            "spec_ref": "ECMA-376 section 17.10.6",
            "description": (
                "titlePg w:val='false' + both default and first-page headers. "
                "Per spec, first-page header should not be active."
            ),
        },
    )


# =========================================================================
# Fixture: first-section-no-header
# =========================================================================

def make_first_section_no_header() -> None:
    """Document where the first section has NO header reference at all.

    ECMA-376 section 17.10.2: If the first section of a document has no
    headerReference, an empty/blank header should be synthesized by the
    application. The model should either synthesize a blank header or
    ensure the first section has an empty header reference.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content - no header.</w:t></w:r>
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
      <w:r><w:t>Section 2 content - has a header.</w:t></w:r>
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
        "first-section-no-header",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Section 2 Header"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "first-section-no-header",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "First section has no headerReference. Per spec, a blank header "
                "should be synthesized. Second section has an explicit header."
            ),
        },
    )


# =========================================================================
# Fixture: single-section-no-header
# =========================================================================

def make_single_section_no_header() -> None:
    """Single-section document with NO header reference at all.

    ECMA-376 section 17.10.2: For the first (and only) section, if no
    headerReference exists, a blank header must be created.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Single section, no header at all.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("single-section-no-header", _build_docx(document_xml), {
        "name": "single-section-no-header",
        "spec_ref": "ECMA-376 section 17.10.2",
        "description": (
            "Single-section document with no headerReference. Per spec, a blank "
            "header should be synthesized."
        ),
    })


# =========================================================================
# Fixture: continuous-section-partial-margins
# =========================================================================

def make_continuous_section_partial_margins() -> None:
    """Multi-section document: Section 1 has full margins, Section 2 is
    continuous with only left margin declared.

    ECMA-376 section 17.6.17: Continuous sections inherit page properties
    from the preceding section. Explicitly declared properties should take
    precedence over inherited values.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 - full margins.</w:t></w:r>
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
      <w:r><w:t>Section 2 - continuous with left=720 only.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:left="720"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("continuous-section-partial-margins", _build_docx(document_xml), {
        "name": "continuous-section-partial-margins",
        "spec_ref": "ECMA-376 section 17.6.17",
        "description": (
            "Section 1: top=1440 bottom=1440 left=1440 right=1440. "
            "Section 2: continuous, only left=720 declared. "
            "Section 2 should get left=720 (own) + top/bottom/right inherited from Section 1."
        ),
    })


# =========================================================================
# Fixture: footer-per-kind-inheritance
# =========================================================================

def make_footer_per_kind_inheritance() -> None:
    """Two sections testing per-kind footer inheritance.

    Section 1 defines default + first footers. Section 2 defines only a
    first-page footer. Per ECMA-376 section 17.10.2, Section 2 should
    inherit the default footer from Section 1 while using its own
    first-page footer.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:headerReference w:type="default" r:id="rId2"/>
          <w:footerReference w:type="default" r:id="rId3"/>
          <w:footerReference w:type="first" r:id="rId4"/>
          <w:titlePg/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:footerReference w:type="first" r:id="rId5"/>
      <w:titlePg/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer3.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer3.xml"/>',
    ])

    _save(
        "footer-per-kind-inheritance",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Default Header"),
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S1 First Footer"),
                "word/footer3.xml": _footer_xml("S2 First Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "footer-per-kind-inheritance",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "S1: default + first footers. S2: only first footer. "
                "S2 should inherit S1's default footer while using its own first footer."
            ),
        },
    )


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section/Story Bug Fixtures --")
    make_next_column_section_type()
    make_title_page_false_with_first_header()
    make_title_page_explicit_false_with_first_header()
    make_first_section_no_header()
    make_single_section_no_header()
    make_continuous_section_partial_margins()
    make_footer_per_kind_inheritance()
    print()


if __name__ == "__main__":
    main()
