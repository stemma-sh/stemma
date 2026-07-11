# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for Section & Story edge-case spec-compliance tests.

Each fixture targets a specific ECMA-376 / ISO 29500-1 behavioral rule
around sections (§17.6), headers/footers (§17.10), and their interactions.

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
    print(f"  section-story-spec/{name}/")


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


def _settings_xml(*, even_and_odd_headers: bool = False) -> str:
    """Build a minimal word/settings.xml."""
    inner = ""
    if even_and_odd_headers:
        inner = f'  <w:evenAndOddHeaders/>\n'
    return f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="{W}" xmlns:r="{R}">
{inner}</w:settings>"""


# =========================================================================
# Fixture 1: titlePg=true, first section, NO first-page header defined
# =========================================================================

def make_title_page_true_no_first_header() -> None:
    """First (and only) section has titlePg=true and a default header, but
    NO first-page header reference.

    ISO 29500-1 §17.10.5: "If no headerReference for the first page header
    is specified and the titlePg element is specified, then the first page
    header shall be inherited from the previous section or, if this is the
    first section in the document, a new blank header shall be created."

    Expected: The model should synthesize a blank First-kind header for
    this section (since there's no previous section to inherit from).
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Body content on title page.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId2"/>
      <w:titlePg/>
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
        "titlepg-true-no-first-header",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Default Header"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "titlepg-true-no-first-header",
            "spec_ref": "ISO 29500-1 §17.10.5",
            "description": (
                "titlePg=true + default header only (no first-page header ref). "
                "Per spec, a blank first-page header shall be created for the "
                "first section."
            ),
        },
    )


# =========================================================================
# Fixture 2: Continuous section without pgSz — inherits page dimensions
# =========================================================================

def make_continuous_no_page_size() -> None:
    """Section 1 has pgSz (letter). Section 2 is continuous with NO pgSz.

    ISO 29500-1 §17.6.22: "continuous section breaks [...] might not specify
    certain page-level section properties, since they shall be inherited from
    the following section."

    Note: The spec says "following section" but implementations (MS Word,
    LibreOffice) inherit from the *preceding* section. Our implementation
    inherits from the preceding section, which matches real-world behavior.

    Expected: Section 2 inherits page_width and page_height from Section 1.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 with letter page size.</w:t></w:r>
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
      <w:r><w:t>Section 2 continuous, no pgSz.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("continuous-no-page-size", _build_docx(document_xml), {
        "name": "continuous-no-page-size",
        "spec_ref": "ISO 29500-1 §17.6.22",
        "description": (
            "Section 1: pgSz w=12240 h=15840 (letter). "
            "Section 2: continuous, NO pgSz element. "
            "Section 2 should inherit page dimensions from Section 1."
        ),
    })


# =========================================================================
# Fixture 3: Even-page header with evenAndOddHeaders, inherited across sections
# =========================================================================

def make_even_header_inheritance() -> None:
    """Section 1 defines default + even headers. Section 2 defines only default.
    evenAndOddHeaders is enabled in settings.xml.

    ISO 29500-1 §17.10.5: "If no headerReference for the even page header is
    specified and the evenAndOddHeaders element is specified, then the even page
    header shall be inherited from the previous section."

    Expected: Section 2 inherits the even header from Section 1, and has
    its own default header.
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
          <w:headerReference w:type="even" r:id="rId3"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId4"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header3.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header3.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>',
    ])

    _save(
        "even-header-inheritance",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("S1 Default Header"),
                "word/header2.xml": _header_xml("S1 Even Header"),
                "word/header3.xml": _header_xml("S2 Default Header"),
                "word/settings.xml": _settings_xml(even_and_odd_headers=True),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "even-header-inheritance",
            "spec_ref": "ISO 29500-1 §17.10.5",
            "description": (
                "evenAndOddHeaders enabled. S1: default + even headers. S2: default only. "
                "S2 should inherit even header from S1."
            ),
        },
    )


# =========================================================================
# Fixture 4: titlePg inheritance — S2 has titlePg=true but no first header ref
# =========================================================================

def make_titlepg_first_header_inheritance() -> None:
    """Section 1 has titlePg=true with default + first headers.
    Section 2 has titlePg=true but only a default header (no first ref).

    ISO 29500-1 §17.10.5: "If no headerReference for the first page header is
    specified and the titlePg element is specified, then the first page header
    shall be inherited from the previous section."

    Expected: Section 2 should inherit the first-page header from Section 1,
    while using its own default header.
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
          <w:headerReference w:type="first" r:id="rId3"/>
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
      <w:headerReference w:type="default" r:id="rId4"/>
      <w:titlePg/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header3.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header3.xml"/>',
    ])

    _save(
        "titlepg-first-header-inheritance",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("S1 Default Header"),
                "word/header2.xml": _header_xml("S1 First Header"),
                "word/header3.xml": _header_xml("S2 Default Header"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "titlepg-first-header-inheritance",
            "spec_ref": "ISO 29500-1 §17.10.5",
            "description": (
                "S1: titlePg=true, default + first headers. "
                "S2: titlePg=true, default header only (no first ref). "
                "S2 should inherit first-page header from S1."
            ),
        },
    )


# =========================================================================
# Fixture 5: nextColumn section type roundtrip
# =========================================================================

def make_next_column_roundtrip() -> None:
    """Document with nextColumn section type in a 2-column layout.

    ECMA-376 §17.6.22 (ST_SectionMark): nextColumn starts the new section
    in the next column. The section_type must survive import -> export.

    This fixture is specifically for verifying roundtrip fidelity: after
    import and re-export, the w:type w:val="nextColumn" must be preserved.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Column 1 content.</w:t></w:r>
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
      <w:r><w:t>Column 2 content.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
      <w:cols w:num="2" w:space="720"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("next-column-roundtrip", _build_docx(document_xml), {
        "name": "next-column-roundtrip",
        "spec_ref": "ECMA-376 §17.6.22 (ST_SectionMark)",
        "description": (
            "Two-column layout with nextColumn section break. "
            "The section_type must survive import -> re-export roundtrip."
        ),
    })


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section/Story Spec Edge-Case Fixtures --")
    make_title_page_true_no_first_header()
    make_continuous_no_page_size()
    make_even_header_inheritance()
    make_titlepg_first_header_inheritance()
    make_next_column_roundtrip()
    print()


if __name__ == "__main__":
    main()
