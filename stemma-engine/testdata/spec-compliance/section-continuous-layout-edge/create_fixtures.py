# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for continuous section break edge cases
confirmed against real Word layout behavior.

Edge cases:
1. Forced break on orientation/page-size mismatch
2. Top/bottom vs left/right margin distinction on shared page
3. Chained continuous sections — page spill determines layout
4. Mixed footnote/endnote settings on shared page

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
    print(f"  section-continuous-layout-edge/{name}/")


# =========================================================================
# Fixture 1a: orientation-mismatch — continuous section with different
# orientation from preceding section (Word promotes to nextPage)
# =========================================================================

def make_orientation_mismatch() -> None:
    """S1(nextPage, portrait) -> S2(continuous, landscape).

    S1: pgSz 12240x15840 (Letter portrait), margins 1440 all
    S2: body-level, continuous, pgSz 15840x12240 orient=landscape

    Word silently promotes S2 to a nextPage break because the orientation
    differs. The XML still says continuous, but the rendering behavior is
    nextPage. At the model level, S2 retains its own page dimensions.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (portrait).</w:t></w:r>
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
      <w:r><w:t>Section 2 content (continuous but landscape).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("orientation-mismatch", _build_docx(document_xml), {
        "name": "orientation-mismatch",
        "spec_ref": "ECMA-376 §17.6.22 / §17.6.14",
        "description": (
            "S1(nextPage, portrait 12240x15840) -> S2(continuous, landscape 15840x12240). "
            "Word promotes S2 to nextPage when orientation differs. "
            "Tests that S2 retains its own page dimensions (not inherited)."
        ),
    })


# =========================================================================
# Fixture 1b: page-size-mismatch — continuous section with different
# page size (same orientation) from preceding section
# =========================================================================

def make_page_size_mismatch() -> None:
    """S1(nextPage, Letter) -> S2(continuous, Legal).

    S1: pgSz 12240x15840 (Letter), margins 1440 all
    S2: body-level, continuous, pgSz 12240x20160 (Legal = taller)

    Word promotes S2 to nextPage when the page size differs.
    At the model level, S2 retains its own declared page size.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (Letter).</w:t></w:r>
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
      <w:r><w:t>Section 2 content (continuous but Legal).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="12240" w:h="20160"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("page-size-mismatch", _build_docx(document_xml), {
        "name": "page-size-mismatch",
        "spec_ref": "ECMA-376 §17.6.22 / §17.6.14",
        "description": (
            "S1(nextPage, Letter 12240x15840) -> S2(continuous, Legal 12240x20160). "
            "Word promotes S2 to nextPage when page height differs. "
            "Tests that S2 retains its own page dimensions."
        ),
    })


# =========================================================================
# Fixture 1c: same-orientation-control — continuous section with same
# orientation and page size (stays continuous)
# =========================================================================

def make_same_orientation_control() -> None:
    """S1(nextPage, portrait) -> S2(continuous, portrait same size).

    S1: pgSz 12240x15840 (Letter portrait), margins 1440 all
    S2: body-level, continuous, pgSz 12240x15840, margins 720

    S2 has the same page size and orientation as S1, so it remains a
    genuine continuous break. S2 keeps its own margins.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (portrait).</w:t></w:r>
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
      <w:r><w:t>Section 2 content (continuous, same size).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("same-orientation-control", _build_docx(document_xml), {
        "name": "same-orientation-control",
        "spec_ref": "ECMA-376 §17.6.22 / §17.6.14",
        "description": (
            "S1(nextPage, portrait 12240x15840) -> S2(continuous, portrait 12240x15840). "
            "Same page size and orientation: S2 remains a true continuous break. "
            "Control test — S2 keeps its own margins."
        ),
    })


# =========================================================================
# Fixture 2: left-right-margins-differ — continuous section with different
# left/right margins from preceding section (allowed mid-page)
# =========================================================================

def make_left_right_margins_differ() -> None:
    """S1(nextPage) -> S2(continuous, different left/right margins).

    S1: margins top=1440 bottom=1440 left=1800 right=1800, pgSz 12240x15840
    S2: body-level, continuous, same pgSz,
        margins top=1440 bottom=1440 left=720 right=720

    On a shared physical page with a continuous break:
    - Top/bottom margins are locked by the preceding section (page-level)
    - Left/right margins CAN change mid-page

    At the model level, S2 has its own explicit margins, so none are
    inherited. This test verifies S2's left/right margins are preserved
    independently of S1's.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (wide margins).</w:t></w:r>
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
      <w:r><w:t>Section 2 content (narrow left/right margins).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="720" w:bottom="1440" w:left="720"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("left-right-margins-differ", _build_docx(document_xml), {
        "name": "left-right-margins-differ",
        "spec_ref": "ECMA-376 §17.6.22 / §17.6.11",
        "description": (
            "S1(nextPage, left=1800 right=1800) -> S2(continuous, left=720 right=720). "
            "Same page size. Left/right margins can change mid-page on a continuous "
            "break. Tests that S2's own left/right margins are preserved."
        ),
    })


# =========================================================================
# Fixture 3: chained-continuous-page-spill — sections with properties
# that would differ across pages when content spills
# =========================================================================

def make_chained_continuous_page_spill() -> None:
    """S1(nextPage) -> S2(continuous) -> S3(continuous) -> S4(continuous, body).

    S1: pgSz 12240x15840, margins 1440 all, cols=1
    S2: continuous, left=720
    S3: continuous, left=360
    S4: body-level, continuous, left=180

    When chained continuous sections spill to a new page, the new page's
    layout comes from the first section that begins on it, NOT the section
    that started the chain. This is a pagination-layer concern; at the
    model level we can verify the sections carry distinct left margins.
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
      <w:r><w:t>Section 2 content (continuous, left=720).</w:t></w:r>
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
      <w:r><w:t>Section 3 content (continuous, left=360).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:type w:val="continuous"/>
          <w:pgMar w:left="360"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 4 content (continuous, left=180).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:type w:val="continuous"/>
      <w:pgMar w:left="180"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("chained-continuous-page-spill", _build_docx(document_xml), {
        "name": "chained-continuous-page-spill",
        "spec_ref": "ECMA-376 §17.6.22",
        "description": (
            "S1(nextPage, left=1440) -> S2(continuous, left=720) -> "
            "S3(continuous, left=360) -> S4(continuous, left=180). "
            "Tests that each section carries distinct left margins through "
            "chained inheritance. Page spill layout is a rendering concern."
        ),
    })


# =========================================================================
# Fixture 4: mixed-footnote-settings — continuous sections with
# independent footnote/endnote settings on shared page
# =========================================================================

def make_mixed_footnote_settings() -> None:
    """S1(nextPage) -> S2(continuous, footnote numFmt=lowerRoman, restart=eachSect)
    -> S3(continuous, body-level, footnote numFmt=decimal, restart=continuous).

    S1: pgSz 12240x15840, margins 1440, footnote numFmt=decimal restart=continuous
    S2: continuous, same pgSz, footnote numFmt=lowerRoman restart=eachSect
    S3: body-level, continuous, footnote numFmt=upperLetter restart=eachPage

    Continuous sections CAN have independent footnote/endnote settings.
    Each section's footnote_pr should be preserved per-section, not
    collapsed during inheritance.
    """
    document_xml = f"""\
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
  <w:body>
    <w:p>
      <w:r><w:t>Section 1 content (decimal footnotes).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:footnotePr>
            <w:numFmt w:val="decimal"/>
            <w:numRestart w:val="continuous"/>
          </w:footnotePr>
          <w:endnotePr>
            <w:numFmt w:val="lowerRoman"/>
          </w:endnotePr>
          <w:type w:val="nextPage"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
                   w:header="720" w:footer="720" w:gutter="0"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 content (lowerRoman footnotes).</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:sectPr>
          <w:footnotePr>
            <w:numFmt w:val="lowerRoman"/>
            <w:numRestart w:val="eachSect"/>
          </w:footnotePr>
          <w:endnotePr>
            <w:numFmt w:val="upperRoman"/>
          </w:endnotePr>
          <w:type w:val="continuous"/>
          <w:pgSz w:w="12240" w:h="15840"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
                   w:header="720" w:footer="720" w:gutter="0"/>
        </w:sectPr>
      </w:pPr>
    </w:p>
    <w:p>
      <w:r><w:t>Section 3 content (upperLetter footnotes).</w:t></w:r>
    </w:p>
    <w:sectPr>
      <w:footnotePr>
        <w:numFmt w:val="upperLetter"/>
        <w:numRestart w:val="eachPage"/>
      </w:footnotePr>
      <w:endnotePr>
        <w:numFmt w:val="decimal"/>
      </w:endnotePr>
      <w:type w:val="continuous"/>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"""

    _save("mixed-footnote-settings", _build_docx(document_xml), {
        "name": "mixed-footnote-settings",
        "spec_ref": "ECMA-376 §17.11.3 / §17.11.2",
        "description": (
            "S1(nextPage, footnote decimal/continuous) -> "
            "S2(continuous, footnote lowerRoman/eachSect) -> "
            "S3(continuous, footnote upperLetter/eachPage). "
            "Continuous sections have independent footnote/endnote settings. "
            "Tests that each section's note properties are preserved per-section."
        ),
    })


# =========================================================================
# Main
# =========================================================================

def main() -> None:
    print("\n-- Section Continuous Layout Edge Case Fixtures --")
    make_orientation_mismatch()
    make_page_size_mismatch()
    make_same_orientation_control()
    make_left_right_margins_differ()
    make_chained_continuous_page_spill()
    make_mixed_footnote_settings()
    print()


if __name__ == "__main__":
    main()
