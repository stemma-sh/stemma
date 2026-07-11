# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-docx",
#     "lxml",
# ]
# ///
"""
Generate DOCX fixtures for section inheritance & settings behavioral tests.

These fixtures exercise:
- Header/footer inheritance across sections (ISO 29500-1 sections 17.10.2, 17.10.5)
- evenAndOddHeaders interaction (ISO 29500-1 sections 17.10.1)
- titlePg inheritance across sections (ISO 29500-1 sections 17.10.6)
- Section type parsing (nextColumn) (ISO 29500-1 sections 17.6.22)
- Footer inheritance across sections (ISO 29500-1 sections 17.10.2)
- Footnote properties per section (ISO 29500-1 sections 17.11.3)
- Section type default behavior (ISO 29500-1 sections 17.6.22)
- Continuous section inheriting page size only when unspecified

Run:  uv run section-inheritance-audit/create_docs.py
"""

import json
import zipfile
import io
from pathlib import Path
from lxml import etree

from docx import Document

ROOT = Path(__file__).parent.parent

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

HEADER_CONTENT_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"
FOOTER_CONTENT_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"
HEADER_REL_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header"
FOOTER_REL_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer"

RELS_NS = "http://schemas.openxmlformats.org/package/2006/relationships"
CT_NS = "http://schemas.openxmlformats.org/package/2006/content-types"


def _build_header_xml(paragraphs: list[str]) -> bytes:
    """Build a w:hdr XML part with the given paragraph texts."""
    root = etree.Element(f"{{{W}}}hdr", nsmap={"w": W, "r": R})
    for text in paragraphs:
        p = etree.SubElement(root, f"{{{W}}}p")
        r = etree.SubElement(p, f"{{{W}}}r")
        t = etree.SubElement(r, f"{{{W}}}t")
        t.text = text
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _build_footer_xml(paragraphs: list[str]) -> bytes:
    """Build a w:ftr XML part with the given paragraph texts."""
    root = etree.Element(f"{{{W}}}ftr", nsmap={"w": W, "r": R})
    for text in paragraphs:
        p = etree.SubElement(root, f"{{{W}}}p")
        r = etree.SubElement(p, f"{{{W}}}r")
        t = etree.SubElement(r, f"{{{W}}}t")
        t.text = text
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def save_fixture(
    area: str,
    name: str,
    data: bytes | io.BytesIO,
    metadata: dict,
    filename: str = "input.docx",
) -> None:
    """Save a fixture file and its metadata."""
    out = ROOT / area / name
    out.mkdir(parents=True, exist_ok=True)
    if isinstance(data, io.BytesIO):
        (out / filename).write_bytes(data.getvalue())
    else:
        (out / filename).write_bytes(data)
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"  {area}/{name}/")


def _get_max_rid(rels_root) -> int:
    """Get the maximum rId number from existing relationships."""
    max_id = 0
    for el in rels_root:
        rid = el.get("Id")
        if rid and rid.startswith("rId"):
            try:
                max_id = max(max_id, int(rid[3:]))
            except ValueError:
                pass
    return max_id


def _base_docx() -> io.BytesIO:
    """Create a base DOCX with minimal content."""
    doc = Document()
    doc.add_paragraph("placeholder")
    buf = io.BytesIO()
    doc.save(buf)
    buf.seek(0)
    return buf


# =========================================================================
# 1. FOOTER INHERITANCE ACROSS SECTIONS (ISO 29500-1 sections 17.10.2)
# =========================================================================

def make_footer_inheritance() -> None:
    """Two sections: S1 defines a default footer, S2 does not.

    Per ISO 29500-1 sections 17.10.2: If a section does not define a footer
    reference, it inherits the footer from the previous section.
    """
    ftr1 = _build_footer_xml(["Section 1 Footer"])

    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        rels_data = zin.read("word/_rels/document.xml.rels")
        rels_root = etree.fromstring(rels_data)
        max_id = _get_max_rid(rels_root)

        rid_ftr1 = f"rId{max_id + 1}"
        rel_el = etree.SubElement(rels_root, f"{{{RELS_NS}}}Relationship")
        rel_el.set("Id", rid_ftr1)
        rel_el.set("Type", FOOTER_REL_TYPE)
        rel_el.set("Target", "footer1.xml")
        new_rels = etree.tostring(rels_root, xml_declaration=True, encoding="UTF-8", standalone=True)

        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "[Content_Types].xml":
                ct_root = etree.fromstring(data)
                pn = "/word/footer1.xml"
                if not ct_root.findall(f"{{{CT_NS}}}Override[@PartName='{pn}']"):
                    ov = etree.SubElement(ct_root, f"{{{CT_NS}}}Override")
                    ov.set("PartName", pn)
                    ov.set("ContentType", FOOTER_CONTENT_TYPE)
                data = etree.tostring(ct_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            elif item.filename == "word/_rels/document.xml.rels":
                data = new_rels

            elif item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: paragraph + sectPr with footer reference
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Section 1 content"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextPage"})
                f1_ref = etree.SubElement(s1, f"{{{W}}}footerReference")
                f1_ref.set(f"{{{R}}}id", rid_ftr1)
                f1_ref.set(f"{{{W}}}type", "default")

                # Section 2: paragraph, body-level sectPr with NO footer
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Section 2 content"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)
        zout.writestr("word/footer1.xml", ftr1)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "footer-inheritance", out_buf, {
        "name": "footer-inheritance",
        "spec_ref": "ISO 29500-1 sections 17.10.2",
        "description": "S1 defines default footer 'Section 1 Footer'; S2 has no footer reference (should inherit from S1)",
        "expected_behavior": "S2 should inherit the footer from S1. footer_refs on body section should include the inherited ref.",
    })


# =========================================================================
# 2. EVEN/ODD HEADERS WITH SETTING OFF (ISO 29500-1 sections 17.10.1)
# =========================================================================

def make_even_headers_setting_off() -> None:
    """Section with even-page header but evenAndOddHeaders setting is OFF.

    Per ISO 29500-1 sections 17.10.1: If evenAndOddHeaders is false and an
    even-page header is specified, it shall be ignored.
    """
    default_hdr = _build_header_xml(["Default Header"])
    even_hdr = _build_header_xml(["Even Page Header"])

    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        rels_data = zin.read("word/_rels/document.xml.rels")
        rels_root = etree.fromstring(rels_data)
        max_id = _get_max_rid(rels_root)

        rid_default = f"rId{max_id + 1}"
        rid_even = f"rId{max_id + 2}"
        for new_rid, fname, rtype in [
            (rid_default, "header1.xml", HEADER_REL_TYPE),
            (rid_even, "header2.xml", HEADER_REL_TYPE),
        ]:
            rel_el = etree.SubElement(rels_root, f"{{{RELS_NS}}}Relationship")
            rel_el.set("Id", new_rid)
            rel_el.set("Type", rtype)
            rel_el.set("Target", fname)
        new_rels = etree.tostring(rels_root, xml_declaration=True, encoding="UTF-8", standalone=True)

        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "[Content_Types].xml":
                ct_root = etree.fromstring(data)
                for i in range(1, 3):
                    pn = f"/word/header{i}.xml"
                    if not ct_root.findall(f"{{{CT_NS}}}Override[@PartName='{pn}']"):
                        ov = etree.SubElement(ct_root, f"{{{CT_NS}}}Override")
                        ov.set("PartName", pn)
                        ov.set("ContentType", HEADER_CONTENT_TYPE)
                data = etree.tostring(ct_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            elif item.filename == "word/_rels/document.xml.rels":
                data = new_rels

            elif item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Page content"

                # Body-level sectPr with both default and even headers
                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                h_def = etree.SubElement(body_sect, f"{{{W}}}headerReference")
                h_def.set(f"{{{R}}}id", rid_default)
                h_def.set(f"{{{W}}}type", "default")
                h_even = etree.SubElement(body_sect, f"{{{W}}}headerReference")
                h_even.set(f"{{{R}}}id", rid_even)
                h_even.set(f"{{{W}}}type", "even")

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            # NO evenAndOddHeaders in settings.xml -- the default is false
            zout.writestr(item, data)
        zout.writestr("word/header1.xml", default_hdr)
        zout.writestr("word/header2.xml", even_hdr)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "even-headers-setting-off", out_buf, {
        "name": "even-headers-setting-off",
        "spec_ref": "ISO 29500-1 sections 17.10.1",
        "description": "Section has both default and even-page headers, but evenAndOddHeaders is absent (off). Even header should be ignored.",
        "expected_behavior": "Only the default header story should be imported. The even header should be filtered out.",
    })


# =========================================================================
# 3. EVEN HEADERS WITH SETTING ON (ISO 29500-1 sections 17.10.1)
# =========================================================================

def make_even_headers_setting_on() -> None:
    """Section with even-page header and evenAndOddHeaders setting is ON.

    Per ISO 29500-1 sections 17.10.1: When evenAndOddHeaders is true, both
    the default (odd) and even-page headers should be active.
    """
    default_hdr = _build_header_xml(["Default Header"])
    even_hdr = _build_header_xml(["Even Page Header"])

    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        rels_data = zin.read("word/_rels/document.xml.rels")
        rels_root = etree.fromstring(rels_data)
        max_id = _get_max_rid(rels_root)

        rid_default = f"rId{max_id + 1}"
        rid_even = f"rId{max_id + 2}"
        for new_rid, fname in [(rid_default, "header1.xml"), (rid_even, "header2.xml")]:
            rel_el = etree.SubElement(rels_root, f"{{{RELS_NS}}}Relationship")
            rel_el.set("Id", new_rid)
            rel_el.set("Type", HEADER_REL_TYPE)
            rel_el.set("Target", fname)
        new_rels = etree.tostring(rels_root, xml_declaration=True, encoding="UTF-8", standalone=True)

        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "[Content_Types].xml":
                ct_root = etree.fromstring(data)
                for i in range(1, 3):
                    pn = f"/word/header{i}.xml"
                    if not ct_root.findall(f"{{{CT_NS}}}Override[@PartName='{pn}']"):
                        ov = etree.SubElement(ct_root, f"{{{CT_NS}}}Override")
                        ov.set("PartName", pn)
                        ov.set("ContentType", HEADER_CONTENT_TYPE)
                data = etree.tostring(ct_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            elif item.filename == "word/_rels/document.xml.rels":
                data = new_rels

            elif item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Page content"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                h_def = etree.SubElement(body_sect, f"{{{W}}}headerReference")
                h_def.set(f"{{{R}}}id", rid_default)
                h_def.set(f"{{{W}}}type", "default")
                h_even = etree.SubElement(body_sect, f"{{{W}}}headerReference")
                h_even.set(f"{{{R}}}id", rid_even)
                h_even.set(f"{{{W}}}type", "even")

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            elif item.filename == "word/settings.xml":
                settings_root = etree.fromstring(data)
                # Add evenAndOddHeaders element
                etree.SubElement(settings_root, f"{{{W}}}evenAndOddHeaders")
                data = etree.tostring(settings_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)
        zout.writestr("word/header1.xml", default_hdr)
        zout.writestr("word/header2.xml", even_hdr)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "even-headers-setting-on", out_buf, {
        "name": "even-headers-setting-on",
        "spec_ref": "ISO 29500-1 sections 17.10.1",
        "description": "Section has both default and even-page headers, and evenAndOddHeaders is ON. Both headers should be active.",
        "expected_behavior": "Both default and even header stories should be imported.",
    })


# =========================================================================
# 4. TITLE PAGE INHERITANCE ACROSS SECTIONS (ISO 29500-1 sections 17.10.6)
# =========================================================================

def make_title_page_inheritance() -> None:
    """S1 has titlePg=true with first+default headers. S2 has no titlePg.

    Per ISO 29500-1 sections 17.10.6: titlePg is per-section. S2 should NOT
    have title_page=true unless it declares it. But the first-page header
    inherited from S1 should still be in S2's header_refs (the inheritance
    is per-kind, not gated by titlePg).
    """
    default_hdr = _build_header_xml(["Default Header"])
    first_hdr = _build_header_xml(["First Page Header"])

    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        rels_data = zin.read("word/_rels/document.xml.rels")
        rels_root = etree.fromstring(rels_data)
        max_id = _get_max_rid(rels_root)

        rid_default = f"rId{max_id + 1}"
        rid_first = f"rId{max_id + 2}"
        for new_rid, fname in [(rid_default, "header1.xml"), (rid_first, "header2.xml")]:
            rel_el = etree.SubElement(rels_root, f"{{{RELS_NS}}}Relationship")
            rel_el.set("Id", new_rid)
            rel_el.set("Type", HEADER_REL_TYPE)
            rel_el.set("Target", fname)
        new_rels = etree.tostring(rels_root, xml_declaration=True, encoding="UTF-8", standalone=True)

        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "[Content_Types].xml":
                ct_root = etree.fromstring(data)
                for i in range(1, 3):
                    pn = f"/word/header{i}.xml"
                    if not ct_root.findall(f"{{{CT_NS}}}Override[@PartName='{pn}']"):
                        ov = etree.SubElement(ct_root, f"{{{CT_NS}}}Override")
                        ov.set("PartName", pn)
                        ov.set("ContentType", HEADER_CONTENT_TYPE)
                data = etree.tostring(ct_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            elif item.filename == "word/_rels/document.xml.rels":
                data = new_rels

            elif item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: titlePg=true, default + first headers
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Section 1 content"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextPage"})
                etree.SubElement(s1, f"{{{W}}}titlePg")
                h_def = etree.SubElement(s1, f"{{{W}}}headerReference")
                h_def.set(f"{{{R}}}id", rid_default)
                h_def.set(f"{{{W}}}type", "default")
                h_first = etree.SubElement(s1, f"{{{W}}}headerReference")
                h_first.set(f"{{{R}}}id", rid_first)
                h_first.set(f"{{{W}}}type", "first")

                # Section 2: NO titlePg, NO headers (inherits from S1)
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Section 2 content"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)
        zout.writestr("word/header1.xml", default_hdr)
        zout.writestr("word/header2.xml", first_hdr)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "title-page-inheritance", out_buf, {
        "name": "title-page-inheritance",
        "spec_ref": "ISO 29500-1 sections 17.10.6",
        "description": "S1 has titlePg=true with default+first headers. S2 has no titlePg and no headers.",
        "expected_behavior": "S1.title_page=Some(true), S2.title_page=None (not inherited). S2 inherits both header refs from S1.",
    })


# =========================================================================
# 5. NEXT COLUMN SECTION TYPE (ISO 29500-1 sections 17.6.22)
# =========================================================================

def make_next_column_section() -> None:
    """Section with type='nextColumn'.

    Per ISO 29500-1 sections 17.6.22: nextColumn is one of five valid section
    break types. It starts the new section in the next column.
    """
    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1 with 2 columns and nextColumn break
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Column 1 content"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextColumn"})
                etree.SubElement(s1, f"{{{W}}}cols", attrib={f"{{{W}}}num": "2", f"{{{W}}}space": "720"})
                etree.SubElement(s1, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})

                # Section 2 content
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Column 2 content"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "next-column-section", out_buf, {
        "name": "next-column-section",
        "spec_ref": "ISO 29500-1 sections 17.6.22",
        "description": "Section with type='nextColumn' break. One of five valid section break types per spec.",
        "expected_behavior": "SectionType should parse as NextColumn (or equivalent). Currently the parser rejects unknown types.",
    })


# =========================================================================
# 6. SECTION TYPE DEFAULT WHEN ABSENT (ISO 29500-1 sections 17.6.22)
# =========================================================================

def make_section_type_absent() -> None:
    """Section without an explicit w:type element.

    Per ISO 29500-1 sections 17.6.22: 'Next page section breaks (the default
    if type is not specified), which begin the new section on the following page.'
    """
    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: has margins but NO w:type element
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Section 1"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                # NO <w:type> element at all
                etree.SubElement(s1, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                etree.SubElement(s1, f"{{{W}}}pgMar", attrib={
                    f"{{{W}}}top": "1440", f"{{{W}}}bottom": "1440",
                    f"{{{W}}}left": "1800", f"{{{W}}}right": "1800",
                    f"{{{W}}}header": "720", f"{{{W}}}footer": "720",
                    f"{{{W}}}gutter": "0",
                })

                # Section 2
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Section 2"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "section-type-absent", out_buf, {
        "name": "section-type-absent",
        "spec_ref": "ISO 29500-1 sections 17.6.22",
        "description": "Section with no explicit w:type element. Per spec, absent type defaults to nextPage.",
        "expected_behavior": "section_type should be None (raw XML has no type element). The rendering layer should treat None as nextPage.",
    })


# =========================================================================
# 7. FOOTNOTE PROPERTIES PER SECTION (ISO 29500-1 sections 17.11.3)
# =========================================================================

def make_footnote_props_per_section() -> None:
    """Two sections with different footnote properties.

    S1: footnotes at pageBottom (default), roman numerals.
    S2: footnotes beneathText, starting at 10.
    """
    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: footnotePr with pos=pageBottom, numFmt=lowerRoman
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Section 1"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextPage"})
                etree.SubElement(s1, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                fn_pr1 = etree.SubElement(s1, f"{{{W}}}footnotePr")
                etree.SubElement(fn_pr1, f"{{{W}}}pos", attrib={f"{{{W}}}val": "pageBottom"})
                etree.SubElement(fn_pr1, f"{{{W}}}numFmt", attrib={f"{{{W}}}val": "lowerRoman"})

                # Section 2: footnotePr with pos=beneathText, numStart=10
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Section 2"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                fn_pr2 = etree.SubElement(body_sect, f"{{{W}}}footnotePr")
                etree.SubElement(fn_pr2, f"{{{W}}}pos", attrib={f"{{{W}}}val": "beneathText"})
                etree.SubElement(fn_pr2, f"{{{W}}}numStart", attrib={f"{{{W}}}val": "10"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "footnote-props-per-section", out_buf, {
        "name": "footnote-props-per-section",
        "spec_ref": "ISO 29500-1 sections 17.11.3",
        "description": "S1: footnotePr pos=pageBottom numFmt=lowerRoman. S2: footnotePr pos=beneathText numStart=10.",
        "expected_behavior": "Each section should have distinct footnote_pr with correct position, format, and start values.",
    })


# =========================================================================
# 8. CONTINUOUS SECTION WITH OWN MARGINS (ISO 29500-1 sections 17.6.17)
# =========================================================================

def make_continuous_with_own_margins() -> None:
    """S1 has wide margins; S2 is continuous with its OWN narrow margins.

    Per ISO 29500-1 sections 17.6.17: continuous sections inherit page-level
    properties only when they don't specify their own. If S2 specifies margins,
    those should NOT be overwritten by inheritance.
    """
    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: wide margins (2880 = 2 inches)
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Wide margins"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                etree.SubElement(s1, f"{{{W}}}pgMar", attrib={
                    f"{{{W}}}top": "2880", f"{{{W}}}bottom": "2880",
                    f"{{{W}}}left": "2880", f"{{{W}}}right": "2880",
                    f"{{{W}}}header": "720", f"{{{W}}}footer": "720",
                    f"{{{W}}}gutter": "0",
                })
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextPage"})

                # Section 2: continuous with NARROW margins (720 = 0.5 inches)
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Narrow margins"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                etree.SubElement(body_sect, f"{{{W}}}pgMar", attrib={
                    f"{{{W}}}top": "720", f"{{{W}}}bottom": "720",
                    f"{{{W}}}left": "720", f"{{{W}}}right": "720",
                    f"{{{W}}}header": "360", f"{{{W}}}footer": "360",
                    f"{{{W}}}gutter": "0",
                })
                etree.SubElement(body_sect, f"{{{W}}}type", attrib={f"{{{W}}}val": "continuous"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "continuous-own-margins", out_buf, {
        "name": "continuous-own-margins",
        "spec_ref": "ISO 29500-1 sections 17.6.17",
        "description": "S1 has wide margins (2880). S2 is continuous with narrow margins (720). S2 should keep its own margins.",
        "expected_behavior": "S2 margin_top=720, margin_left=720 (not overwritten by S1's 2880).",
    })


# =========================================================================
# 9. MIXED HEADER AND FOOTER INHERITANCE (ISO 29500-1 sections 17.10.2, 17.10.5)
# =========================================================================

def make_mixed_header_footer_inheritance() -> None:
    """S1 declares default header + default footer. S2 declares only a first header.

    Per spec: S2 inherits default header from S1, inherits default footer from S1,
    and has its own first header. The inheritance is per-kind per-story-type.
    """
    default_hdr = _build_header_xml(["Section 1 Default Header"])
    default_ftr = _build_footer_xml(["Section 1 Default Footer"])
    first_hdr = _build_header_xml(["Section 2 First Header"])

    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        rels_data = zin.read("word/_rels/document.xml.rels")
        rels_root = etree.fromstring(rels_data)
        max_id = _get_max_rid(rels_root)

        rid_hdr1 = f"rId{max_id + 1}"
        rid_ftr1 = f"rId{max_id + 2}"
        rid_hdr2 = f"rId{max_id + 3}"

        for new_rid, fname, rtype in [
            (rid_hdr1, "header1.xml", HEADER_REL_TYPE),
            (rid_ftr1, "footer1.xml", FOOTER_REL_TYPE),
            (rid_hdr2, "header2.xml", HEADER_REL_TYPE),
        ]:
            rel_el = etree.SubElement(rels_root, f"{{{RELS_NS}}}Relationship")
            rel_el.set("Id", new_rid)
            rel_el.set("Type", rtype)
            rel_el.set("Target", fname)
        new_rels = etree.tostring(rels_root, xml_declaration=True, encoding="UTF-8", standalone=True)

        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "[Content_Types].xml":
                ct_root = etree.fromstring(data)
                for pn, ct in [
                    ("/word/header1.xml", HEADER_CONTENT_TYPE),
                    ("/word/footer1.xml", FOOTER_CONTENT_TYPE),
                    ("/word/header2.xml", HEADER_CONTENT_TYPE),
                ]:
                    if not ct_root.findall(f"{{{CT_NS}}}Override[@PartName='{pn}']"):
                        ov = etree.SubElement(ct_root, f"{{{CT_NS}}}Override")
                        ov.set("PartName", pn)
                        ov.set("ContentType", ct)
                data = etree.tostring(ct_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            elif item.filename == "word/_rels/document.xml.rels":
                data = new_rels

            elif item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: default header + default footer
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Section 1"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextPage"})
                h1 = etree.SubElement(s1, f"{{{W}}}headerReference")
                h1.set(f"{{{R}}}id", rid_hdr1)
                h1.set(f"{{{W}}}type", "default")
                f1 = etree.SubElement(s1, f"{{{W}}}footerReference")
                f1.set(f"{{{R}}}id", rid_ftr1)
                f1.set(f"{{{W}}}type", "default")

                # Section 2: only first header + titlePg, no footer
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Section 2"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}titlePg")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                h2 = etree.SubElement(body_sect, f"{{{W}}}headerReference")
                h2.set(f"{{{R}}}id", rid_hdr2)
                h2.set(f"{{{W}}}type", "first")

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)
        zout.writestr("word/header1.xml", default_hdr)
        zout.writestr("word/footer1.xml", default_ftr)
        zout.writestr("word/header2.xml", first_hdr)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "mixed-header-footer-inheritance", out_buf, {
        "name": "mixed-header-footer-inheritance",
        "spec_ref": "ISO 29500-1 sections 17.10.2, 17.10.5",
        "description": "S1: default header + default footer. S2: only first header. S2 should inherit default header + default footer from S1.",
        "expected_behavior": "S2 header_refs: [first(own), default(inherited)]. S2 footer_refs: [default(inherited)].",
    })


# =========================================================================
# 10. ENDNOTE PROPERTIES PER SECTION (ISO 29500-1 sections 17.11.2)
# =========================================================================

def make_endnote_props_per_section() -> None:
    """Section with endnote position set to 'sectEnd'.

    Per ISO 29500-1 sections 17.11.2: endnotes can be positioned per-section
    at either the end of the section (sectEnd) or end of document (docEnd).
    """
    buf = _base_docx()
    out_buf = io.BytesIO()

    with zipfile.ZipFile(buf, "r") as zin, zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED) as zout:
        for item in zin.infolist():
            data = zin.read(item.filename)

            if item.filename == "word/document.xml":
                doc_root = etree.fromstring(data)
                body = doc_root.find(f"{{{W}}}body")
                for child in list(body):
                    body.remove(child)

                # Section 1: endnotePr with pos=sectEnd, numFmt=upperRoman
                p1 = etree.SubElement(body, f"{{{W}}}p")
                r1 = etree.SubElement(p1, f"{{{W}}}r")
                t1 = etree.SubElement(r1, f"{{{W}}}t")
                t1.text = "Section 1"

                p1b = etree.SubElement(body, f"{{{W}}}p")
                pPr1 = etree.SubElement(p1b, f"{{{W}}}pPr")
                s1 = etree.SubElement(pPr1, f"{{{W}}}sectPr")
                etree.SubElement(s1, f"{{{W}}}type", attrib={f"{{{W}}}val": "nextPage"})
                etree.SubElement(s1, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                en_pr = etree.SubElement(s1, f"{{{W}}}endnotePr")
                etree.SubElement(en_pr, f"{{{W}}}pos", attrib={f"{{{W}}}val": "sectEnd"})
                etree.SubElement(en_pr, f"{{{W}}}numFmt", attrib={f"{{{W}}}val": "upperRoman"})

                # Section 2: endnotePr with pos=docEnd
                p2 = etree.SubElement(body, f"{{{W}}}p")
                r2 = etree.SubElement(p2, f"{{{W}}}r")
                t2 = etree.SubElement(r2, f"{{{W}}}t")
                t2.text = "Section 2"

                body_sect = etree.SubElement(body, f"{{{W}}}sectPr")
                etree.SubElement(body_sect, f"{{{W}}}pgSz",
                                 attrib={f"{{{W}}}w": "12240", f"{{{W}}}h": "15840"})
                en_pr2 = etree.SubElement(body_sect, f"{{{W}}}endnotePr")
                etree.SubElement(en_pr2, f"{{{W}}}pos", attrib={f"{{{W}}}val": "docEnd"})

                data = etree.tostring(doc_root, xml_declaration=True, encoding="UTF-8", standalone=True)

            zout.writestr(item, data)

    out_buf.seek(0)
    save_fixture("section-inheritance-audit", "endnote-props-per-section", out_buf, {
        "name": "endnote-props-per-section",
        "spec_ref": "ISO 29500-1 sections 17.11.2",
        "description": "S1: endnotePr pos=sectEnd numFmt=upperRoman. S2: endnotePr pos=docEnd.",
        "expected_behavior": "Each section should have distinct endnote_pr with correct position and format.",
    })


# =========================================================================

def main() -> None:
    print("Generating section inheritance audit fixtures:")
    make_footer_inheritance()
    make_even_headers_setting_off()
    make_even_headers_setting_on()
    make_title_page_inheritance()
    make_next_column_section()
    make_section_type_absent()
    make_footnote_props_per_section()
    make_continuous_with_own_margins()
    make_mixed_header_footer_inheritance()
    make_endnote_props_per_section()
    print("Done.")


if __name__ == "__main__":
    main()
