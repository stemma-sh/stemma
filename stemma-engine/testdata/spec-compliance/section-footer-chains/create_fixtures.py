# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for footer inheritance chain tests.

Each fixture exercises footer inheritance across 3+ sections per
ECMA-376 section 17.10.2.

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
    print(f"  section-footer-chains/{name}/")


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
# Fixture 1: three-section-footer-inheritance
# =========================================================================

def make_three_section_footer_inheritance() -> None:
    """Three sections with partial footer declarations to test per-kind inheritance.

    - S1: declares default footer (footer1.xml) + first footer (footer2.xml), titlePg=true
    - S2: declares only first footer (footer3.xml), titlePg=true — inherits default from S1
    - S3 (body sectPr): declares nothing, titlePg=true — inherits both from S2
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
          <w:footerReference w:type="default" r:id="rId2"/>
          <w:footerReference w:type="first" r:id="rId3"/>
          <w:titlePg/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:footerReference w:type="first" r:id="rId4"/>
          <w:titlePg/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 3 content.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:titlePg/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer3.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer3.xml"/>',
    ])

    _save(
        "three-section-footer-inheritance",
        _build_docx(
            document_xml,
            extra_parts={
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S1 First Footer"),
                "word/footer3.xml": _footer_xml("S2 First Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "three-section-footer-inheritance",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "S1: default + first footers (titlePg=true). "
                "S2: only first footer (titlePg=true) — inherits default from S1. "
                "S3: nothing (titlePg=true) — inherits both from S2."
            ),
        },
    )


# =========================================================================
# Fixture 2: three-section-footer-override-chain
# =========================================================================

def make_three_section_footer_override_chain() -> None:
    """Three sections testing that an override in S2 is what S3 inherits.

    - S1: declares default footer (footer1.xml)
    - S2: declares default footer (footer2.xml) — overrides S1
    - S3 (body sectPr): declares nothing — should inherit S2's default (NOT S1's)
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
          <w:footerReference w:type="default" r:id="rId2"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:footerReference w:type="default" r:id="rId3"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 3 content.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    extra_overrides = "\n".join([
        '  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
        '  <Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>',
    ])
    extra_word_rels = "\n".join([
        '  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
    ])

    _save(
        "three-section-footer-override-chain",
        _build_docx(
            document_xml,
            extra_parts={
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S2 Default Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "three-section-footer-override-chain",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "S1: default footer. S2: default footer (overrides S1). "
                "S3: nothing — should inherit S2's default footer (NOT S1's)."
            ),
        },
    )


# =========================================================================
# Fixture 3: mixed-header-footer-three-sections
# =========================================================================

def make_mixed_header_footer_three_sections() -> None:
    """Three sections testing mixed header/footer inheritance.

    - S1: declares default header (header1.xml) + default footer (footer1.xml)
    - S2: declares default header (header2.xml) only — inherits footer from S1
    - S3 (body sectPr): declares default footer (footer2.xml) only — inherits header from S2
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
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:headerReference w:type="default" r:id="rId4"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 3 content.</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:footerReference w:type="default" r:id="rId5"/>
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
        '  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>',
        '  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>',
        '  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/>',
    ])

    _save(
        "mixed-header-footer-three-sections",
        _build_docx(
            document_xml,
            extra_parts={
                "word/header1.xml": _header_xml("S1 Default Header"),
                "word/header2.xml": _header_xml("S2 Default Header"),
                "word/footer1.xml": _footer_xml("S1 Default Footer"),
                "word/footer2.xml": _footer_xml("S3 Default Footer"),
            },
            extra_overrides=extra_overrides,
            extra_word_rels=extra_word_rels,
        ),
        {
            "name": "mixed-header-footer-three-sections",
            "spec_ref": "ECMA-376 section 17.10.2",
            "description": (
                "S1: default header + default footer. "
                "S2: default header only — inherits footer from S1. "
                "S3: default footer only — inherits header from S2."
            ),
        },
    )


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section Footer Chain Fixtures --")
    make_three_section_footer_inheritance()
    make_three_section_footer_override_chain()
    make_mixed_header_footer_three_sections()
    print()


if __name__ == "__main__":
    main()
