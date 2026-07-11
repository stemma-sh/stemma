# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for titlePg + First-kind header/footer inheritance
interaction tests.

These fixtures test the subtle interaction between:
1. resolve_section_header_inheritance (copies refs across sections per-kind)
2. filter_first_page_headers_footers (removes First-kind refs from sections
   without titlePg=true)

The pipeline runs inheritance BEFORE titlePg filtering, which means:
- Intermediate sections without titlePg can still serve as inheritance conduits
  for First-kind refs
- First-kind stories are only garbage-collected from doc.headers/doc.footers
  when NO section has titlePg=true

Per ISO 29500-1 sections 17.10.6 + 17.10.2.

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
    print(f"  section-titlepg-interaction/{name}/")


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
# Fixture 1: titlepg-gap-inheritance
# =========================================================================

def make_titlepg_gap_inheritance() -> None:
    """Three sections testing First-kind inheritance through a non-titlePg gap.

    S1 (mid-doc): titlePg=true, declares Default header + First header
    S2 (mid-doc): NO titlePg, declares nothing -- inherits from S1
    S3 (body-level): titlePg=true, declares nothing -- inherits from S2

    Pipeline flow:
    1. After resolve_section_header_inheritance:
       - S1: Default + First (own)
       - S2: Default + First (inherited from S1)
       - S3: Default + First (inherited from S2)
    2. After filter_first_page_headers_footers:
       - S1: Default + First (titlePg=true, kept)
       - S2: Default only (no titlePg, First filtered)
       - S3: Default + First (titlePg=true, kept)

    S3 should end up with S1's First header ref, even though S2 (the
    intermediate section) doesn't have titlePg. The inheritance happened
    before filtering.
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
          <w:footerReference w:type="default" r:id="rId4"/>
          <w:footerReference w:type="first" r:id="rId5"/>
          <w:titlePg/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (no titlePg).</w:t></w:r>
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
      <w:r><w:t>Section 3 content (titlePg, no own headers).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:titlePg/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
    ])

    _save(
        "titlepg-gap-inheritance",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("S1 Default Header"),
                "word/header2.xml": _header_xml("S1 First Header"),
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S1 First Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "titlepg-gap-inheritance",
            "spec_ref": "ISO 29500-1 sections 17.10.6 + 17.10.2",
            "description": (
                "S1: titlePg=true with Default+First headers+footers. "
                "S2: no titlePg, no declarations. "
                "S3: titlePg=true, no declarations. "
                "Tests First-kind inheritance through a non-titlePg gap section."
            ),
        },
    )


# =========================================================================
# Fixture 2: titlepg-selective-first-kinds
# =========================================================================

def make_titlepg_selective_first_kinds() -> None:
    """Two sections testing selective First-kind inheritance for mixed header/footer.

    S1 (mid-doc): titlePg=true, declares Default header + First header +
                   Default footer + First footer
    S2 (body-level): titlePg=true, declares ONLY Default footer (overrides S1's)
                   -- should inherit Default header, First header, and First
                   footer from S1

    This verifies that per-kind inheritance correctly handles partial
    overrides when titlePg is active on both sections.
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
          <w:footerReference w:type="default" r:id="rId4"/>
          <w:footerReference w:type="first" r:id="rId5"/>
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
      <w:footerReference w:type="default" r:id="rId6"/>
      <w:titlePg/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer3.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
        '  <Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer3.xml"/>',
    ])

    _save(
        "titlepg-selective-first-kinds",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("S1 Default Header"),
                "word/header2.xml": _header_xml("S1 First Header"),
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S1 First Footer"),
                "word/footer3.xml": _footer_xml("S2 Default Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "titlepg-selective-first-kinds",
            "spec_ref": "ISO 29500-1 sections 17.10.6 + 17.10.2",
            "description": (
                "S1: titlePg + Default header + First header + Default footer + First footer. "
                "S2: titlePg + only Default footer (overrides S1's). "
                "S2 should inherit Default header, First header, and First footer from S1."
            ),
        },
    )


# =========================================================================
# Fixture 3: no-section-has-titlepg-first-stories-removed
# =========================================================================

def make_no_section_has_titlepg() -> None:
    """Two sections, neither has titlePg. First-kind stories should be removed entirely.

    S1 (mid-doc): NO titlePg, declares Default header + First header +
                   Default footer + First footer
    S2 (body-level): NO titlePg, declares nothing

    Since NO section has titlePg=true:
    - All First-kind refs should be filtered from all sections
    - All First-kind stories should be removed from doc.headers and doc.footers
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (no titlePg).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:headerReference w:type="default" r:id="rId2"/>
          <w:headerReference w:type="first" r:id="rId3"/>
          <w:footerReference w:type="default" r:id="rId4"/>
          <w:footerReference w:type="first" r:id="rId5"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (no titlePg).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
    ])

    _save(
        "no-section-has-titlepg-first-stories-removed",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("Default Header"),
                "word/header2.xml": _header_xml("First Header"),
                "word/footer1.xml": _footer_xml("Default Footer"),
                "word/footer2.xml": _footer_xml("First Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "no-section-has-titlepg-first-stories-removed",
            "spec_ref": "ISO 29500-1 section 17.10.6",
            "description": (
                "S1: no titlePg, declares Default+First headers+footers. "
                "S2: no titlePg, no declarations. "
                "No section has titlePg, so ALL First-kind stories should be removed entirely."
            ),
        },
    )


# =========================================================================
# Fixture 4: titlepg-only-on-later-section
# =========================================================================

def make_titlepg_only_on_later_section() -> None:
    """First-kind story retained because a later section uses titlePg.

    S1 (mid-doc): NO titlePg, declares Default header + First header +
                   Default footer + First footer
    S2 (body-level): titlePg=true, declares nothing -- inherits from S1

    S2 should keep the inherited First-kind refs (has titlePg).
    S1 should NOT have First-kind refs (no titlePg).
    doc.headers/doc.footers should STILL have First-kind stories because
    S2 has titlePg.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (no titlePg, declares First).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:headerReference w:type="default" r:id="rId2"/>
          <w:headerReference w:type="first" r:id="rId3"/>
          <w:footerReference w:type="default" r:id="rId4"/>
          <w:footerReference w:type="first" r:id="rId5"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (titlePg, no own headers).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:titlePg/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>',
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
    ])

    _save(
        "titlepg-only-on-later-section",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("S1 Default Header"),
                "word/header2.xml": _header_xml("S1 First Header"),
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S1 First Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "titlepg-only-on-later-section",
            "spec_ref": "ISO 29500-1 sections 17.10.6 + 17.10.2",
            "description": (
                "S1: no titlePg, declares Default+First headers+footers. "
                "S2: titlePg=true, no declarations, inherits from S1. "
                "S2 keeps First refs (titlePg), S1 loses them. "
                "First-kind stories preserved in doc because S2 has titlePg."
            ),
        },
    )


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section titlePg Interaction Fixtures --")
    make_titlepg_gap_inheritance()
    make_titlepg_selective_first_kinds()
    make_no_section_has_titlepg()
    make_titlepg_only_on_later_section()
    print()


if __name__ == "__main__":
    main()
