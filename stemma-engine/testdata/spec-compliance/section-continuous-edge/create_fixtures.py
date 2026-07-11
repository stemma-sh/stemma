# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for continuous section inheritance edge cases.

Each fixture exercises a specific edge case in how continuous sections
inherit page-level properties from preceding sections per ISO 29500-1
section 17.6.17.

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
                zf.writestr(path, content)
    return buf.getvalue()


def _save(name: str, docx_bytes: bytes, metadata: dict) -> None:
    out = ROOT / name
    out.mkdir(parents=True, exist_ok=True)
    (out / "input.docx").write_bytes(docx_bytes)
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"  section-continuous-edge/{name}/")


# =========================================================================
# Fixture 1: chained-continuous-sections
# =========================================================================

def make_chained_continuous_sections() -> None:
    """S1(nextPage) -> S2(continuous, partial margins) -> S3(continuous, no margins).

    S1: margins top=1440 left=1800 bottom=1440 right=1800, pgSz 12240x15840
    S2: continuous, only left=720 declared, no pgSz
    S3: body-level, continuous, NO pgMar at all, no pgSz

    After propagation:
    - S2: left=720 (own), top/bottom=1440, right=1800 (inherited from S1),
           page_width=12240, page_height=15840 (inherited from S1)
    - S3: inherits ALL from S2's enriched state:
           left=720, top=1440, bottom=1440, right=1800,
           page_width=12240, page_height=15840
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
          <w:type w:val="nextPage"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800"
                   w:header="720" w:footer="720" w:gutter="0"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (continuous, partial).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:type w:val="continuous"/>
          <w:pgMar w:left="720"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 3 content (continuous, empty).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("chained-continuous-sections", _build_docx(document_xml), {
        "name": "chained-continuous-sections",
        "spec_ref": "ISO 29500-1 section 17.6.17",
        "description": (
            "S1(nextPage, full margins+pgSz) -> S2(continuous, left=720 only) -> "
            "S3(continuous, no pgMar/pgSz). Tests that S3 inherits from S2's "
            "enriched state (S1's values flow through S2 to S3)."
        ),
    })


# =========================================================================
# Fixture 2: non-continuous-breaks-chain
# =========================================================================

def make_non_continuous_breaks_chain() -> None:
    """S1(nextPage) -> S2(continuous) -> S3(nextPage) -> S4(continuous).

    S1: margins 1440 all, pgSz 12240x15840
    S2: continuous, no margins -> inherits all from S1
    S3: nextPage, margins 2880 all, pgSz 15840x12240 (landscape)
    S4: body-level, continuous, no margins -> should inherit from S3 (not S2!)

    Tests that a nextPage section breaks the chain:
    S4 gets S3's values, not S1's or S2's.
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
          <w:type w:val="nextPage"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
                   w:header="720" w:footer="720" w:gutter="0"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (continuous, inherits from S1).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:type w:val="continuous"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 3 content (nextPage, own margins).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:type w:val="nextPage"/>
          <w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/>
          <w:pgMar w:top="2880" w:right="2880" w:bottom="2880" w:left="2880"
                   w:header="1440" w:footer="1440" w:gutter="360"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 4 content (continuous, inherits from S3).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("non-continuous-breaks-chain", _build_docx(document_xml), {
        "name": "non-continuous-breaks-chain",
        "spec_ref": "ISO 29500-1 section 17.6.17",
        "description": (
            "S1(nextPage, 1440 margins) -> S2(continuous, no margins) -> "
            "S3(nextPage, 2880 margins, landscape) -> S4(continuous, no margins). "
            "S4 should inherit from S3, not from S2. Tests that nextPage breaks "
            "the inheritance chain."
        ),
    })


# =========================================================================
# Fixture 3: page-size-inheritance
# =========================================================================

def make_page_size_inheritance() -> None:
    """S1(nextPage) has pgSz, S2(continuous) has NO pgSz element at all.

    S1: pgSz w=12240 h=15840, margins 1440 all
    S2: body-level, continuous, margins 720 all, NO pgSz element

    Expected: S2 inherits page_width=12240 and page_height=15840 from S1.
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
          <w:type w:val="nextPage"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
                   w:header="720" w:footer="720" w:gutter="0"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (continuous, has margins but no pgSz).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("page-size-inheritance", _build_docx(document_xml), {
        "name": "page-size-inheritance",
        "spec_ref": "ISO 29500-1 section 17.6.17",
        "description": (
            "S1(nextPage, pgSz 12240x15840, margins 1440) -> "
            "S2(continuous, margins 720, NO pgSz element). "
            "S2 should inherit page_width=12240 and page_height=15840 from S1."
        ),
    })


# =========================================================================
# Fixture 4: gutter-and-distances
# =========================================================================

def make_gutter_and_distances() -> None:
    """S1(nextPage) has gutter/header/footer distances, S2(continuous) omits them.

    S1: pgMar with gutter=720, header=720, footer=720, margins 1440 all
    S2: body-level, continuous, pgMar with only top/left/bottom/right declared,
        NO gutter/header/footer attributes

    Expected: S2 inherits gutter=720, header_distance=720, footer_distance=720.
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
          <w:type w:val="nextPage"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
                   w:header="720" w:footer="720" w:gutter="720"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (continuous, margins only, no gutter/distances).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("gutter-and-distances", _build_docx(document_xml), {
        "name": "gutter-and-distances",
        "spec_ref": "ISO 29500-1 section 17.6.17",
        "description": (
            "S1(nextPage, gutter=720, header=720, footer=720) -> "
            "S2(continuous, margins declared but NO gutter/header/footer attributes). "
            "S2 should inherit gutter=720, header_distance=720, footer_distance=720."
        ),
    })


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section Continuous Edge Case Fixtures --")
    make_chained_continuous_sections()
    make_non_continuous_breaks_chain()
    make_page_size_inheritance()
    make_gutter_and_distances()
    print()


if __name__ == "__main__":
    main()
