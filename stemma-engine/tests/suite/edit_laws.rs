//! L4 Edit terminals and locality, for every operation in the catalog.
//!
//! For each canonical shape in the operation catalog, the suite finds a fixture
//! document the operation applies to, fills the shape's placeholders from that
//! document, and runs it as a tracked change and directly. Then:
//!
//! - **Reject All restores the source**, when the tracked edit created
//!   revisions.
//! - **Accept All equals the direct edit**, when it created revisions.
//! - **A tracked edit without revisions is the direct edit:** comments,
//!   bookmarks, content controls, image size or layout and document defaults
//!   have no revision carrier in Word, so both modes must agree.
//! - **Untouched blocks stay untouched:** a content operation changes only the
//!   blocks it names or that hold the object it names.
//! - **One spelling:** the edited document is the same model before and after
//!   it is saved and reopened.
//!
//! Operations that exist only as tracked changes (`move`) are checked in that
//! mode alone. Readings are compared after save and reopen, without proofing
//! markers and run boundaries, as in the IR laws. Besides the engine fixtures,
//! one synthetic document supplies objects no fixture contains (a text box, a
//! floating picture, a legacy form field, content controls). An operation no
//! document can host fails: the suite must cover the whole catalog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use stemma::ExportOptions;
use stemma::api::{Document, Resolution};
use stemma::domain::{
    BlockNode, CanonDoc, DecorationType, FieldKind, InlineNode, NoteType, OpaqueKind, TrackedBlock,
};
use stemma::edit_v4::catalog::operation_catalog;
use stemma::edit_v4::parse_transaction;
use stemma::roundtrip_compare::{compare_canon_docs, compare_tracked_block_pair};

const NEW_TEXT: &str = "Laws edit text";
const OMML: &str = r#"<m:oMath xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\"><m:r><m:t>x</m:t></m:r></m:oMath>"#;

/// Operations whose effect is document-wide by design (styles, defaults,
/// sections, numbering, block insertion): the locality check does not apply.
fn is_global(op: &str) -> bool {
    matches!(
        op,
        "create_style"
            | "modify_style"
            | "set_doc_defaults"
            | "set_page_setup"
            | "set_section_type"
            | "insert_section_break"
            | "create_header"
            | "create_footer"
            | "set_header_footer_mode"
            | "set_numbering"
            | "insert"
    )
}

// ── Documents ───────────────────────────────────────────────────────────

fn fixture_documents() -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read fixture directory") {
            let path = entry.expect("fixture entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "docx")
            {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    // Small documents first: a failure is easiest to read on the smallest host.
    paths.sort_by_key(|path| std::fs::metadata(path).map_or(u64::MAX, |m| m.len()));
    let mut documents: Vec<(String, Vec<u8>)> = paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).expect("read fixture");
            (
                path.strip_prefix(&root)
                    .expect("under testdata")
                    .display()
                    .to_string(),
                bytes,
            )
        })
        .collect();
    documents.push(("synthetic objects document".to_string(), objects_docx()));
    documents
}

const PNG_1X1: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0, 3, 1,
    1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// A document holding the objects no engine fixture contains: a text box and a
/// floating picture (anchored drawings), a legacy text form field, a checkbox
/// and a plain-text content control, and a body-level content control.
fn objects_docx() -> Vec<u8> {
    use std::io::Write;
    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#;
    let anchor = |id: u32, graphic: &str| {
        format!(
            r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="{id}" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="{id}" name="Object {id}"/><wp:cNvGraphicFramePr/><a:graphic>{graphic}</a:graphic></wp:anchor></w:drawing></w:r>"#
        )
    };
    let picture = anchor(
        1,
        r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="pixel.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rIdImage"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData>"#,
    );
    let textbox = anchor(
        2,
        r#"<a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Textbox original words</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData>"#,
    );
    let form_field = r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Text1"/><w:enabled/><w:calcOnExit w:val="0"/><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText xml:space="preserve"> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Field value</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
    let checkbox = r#"<w:sdt><w:sdtPr><w:id w:val="101"/><w14:checkbox><w14:checked w14:val="0"/><w14:checkedState w14:val="2612" w14:font="MS Gothic"/><w14:uncheckedState w14:val="2610" w14:font="MS Gothic"/></w14:checkbox></w:sdtPr><w:sdtContent><w:r><w:t>☐</w:t></w:r></w:sdtContent></w:sdt>"#;
    let plain_control = r#"<w:sdt><w:sdtPr><w:id w:val="102"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t>Control words</w:t></w:r></w:sdtContent></w:sdt>"#;
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {NS}><w:body><w:p><w:r><w:t xml:space="preserve">Picture paragraph words </w:t></w:r>{picture}</w:p><w:p><w:r><w:t xml:space="preserve">Textbox paragraph words </w:t></w:r>{textbox}</w:p><w:p><w:r><w:t xml:space="preserve">Form paragraph words </w:t></w:r>{form_field}</w:p><w:p><w:r><w:t xml:space="preserve">Checkbox paragraph words </w:t></w:r>{checkbox}</w:p><w:p><w:r><w:t xml:space="preserve">Control paragraph words </w:t></w:r>{plain_control}</w:p><w:sdt><w:sdtPr><w:id w:val="103"/><w:text/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Body control words</w:t></w:r></w:p></w:sdtContent></w:sdt><w:sdt><w:sdtPr><w:id w:val="104"/><w:alias w:val="Form block"/></w:sdtPr><w:sdtContent><w:customXml w:uri="urn:x" w:element="sealed"><w:p><w:r><w:t>Form block intro</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="BlockText"/><w:enabled/><w:calcOnExit w:val="0"/><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText xml:space="preserve"> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Block old value</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:customXml></w:sdtContent></w:sdt><w:p><w:r><w:t>Closing paragraph words</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/></w:sectPr></w:body></w:document>"#
    );
    // A running label in a header textbox, the paragraph holding no text of
    // its own.
    let header_textbox = anchor(
        3,
        r#"<a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Header label words</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData>"#,
    );
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr {NS}><w:p>{header_textbox}</w:p></w:hdr>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let package_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/pixel.png"/><Relationship Id="rIdHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: zip::write::FileOptions = zip::write::FileOptions::default();
        for (path, content) in [
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", package_rels.as_bytes()),
            ("word/document.xml", document.as_bytes()),
            ("word/_rels/document.xml.rels", document_rels.as_bytes()),
            ("word/media/pixel.png", PNG_1X1),
            ("word/header1.xml", header.as_bytes()),
        ] {
            zip.start_file(path, options).expect("start part");
            zip.write_all(content).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    bytes
}

// ── What a document offers ──────────────────────────────────────────────

fn text_of<'a>(inlines: impl Iterator<Item = &'a InlineNode>) -> String {
    inlines
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// A word that occurs exactly once in `text`, at least four letters long.
fn unique_word(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .find(|word| text.matches(word).count() == 1)
        .map(str::to_string)
}

fn raw_text(raw: &[u8]) -> String {
    let xml = String::from_utf8_lossy(raw);
    let mut out = String::new();
    for piece in xml.split("<w:t").skip(1) {
        if let Some(start) = piece.find('>')
            && let Some(end) = piece[start..].find("</w:t>")
        {
            out.push_str(&piece[start + 1..start + end]);
            out.push(' ');
        }
    }
    out
}

struct Drawing {
    paragraph: String,
    id: String,
    textbox: bool,
    anchored: bool,
}

/// Everything a shape may need, found in one document.
#[derive(Default)]
struct Host {
    /// Non-empty body paragraphs, in order: (id, text).
    paragraphs: Vec<(String, String)>,
    /// (id, the text of each grid column's top cell)
    tables: Vec<(String, Vec<String>)>,
    /// (paragraph, id, display text, url)
    hyperlinks: Vec<(String, String, String, Option<String>)>,
    drawings: Vec<Drawing>,
    /// (paragraph, id, is checkbox)
    controls: Vec<(String, String, bool)>,
    /// (paragraph, form field id)
    form_fields: Vec<(String, String)>,
    /// (paragraph, opaque id, a word inside it)
    opaques_with_text: Vec<(String, String, String)>,
    /// (paragraph, name)
    bookmarks: Vec<(String, String)>,
    /// Body indices of sealed body-level content controls (preserved
    /// opaque blocks).
    body_controls: Vec<usize>,
    /// First block ids of open body-level content controls.
    open_controls: Vec<String>,
    /// A FORMTEXT field inside a body control: (body index, paragraph index,
    /// field index, containing-block hash).
    block_form_text: Option<(usize, usize, usize, String)>,
    comments: Vec<String>,
    footnotes: Vec<String>,
    /// (part file name, paragraph id, a word in it)
    header: Option<(String, String, String)>,
    /// A textbox in a header: (part file name, paragraph id, opaque id, a
    /// word inside it).
    header_textbox: Option<(String, String, String, String)>,
    footer: Option<(String, String, String)>,
}

impl Host {
    fn read(canonical: &CanonDoc) -> Host {
        let mut host = Host::default();
        for (index, tracked) in canonical.blocks.iter().enumerate() {
            if tracked.block_sdt_wrap.is_some()
                && let BlockNode::Paragraph(p) = &tracked.block
            {
                host.open_controls.push(p.id.to_string());
            }
            match &tracked.block {
                BlockNode::Paragraph(p) => {
                    let id = p.id.to_string();
                    let text = text_of(p.segments.iter().flat_map(|s| &s.inlines));
                    if !text.trim().is_empty() {
                        host.paragraphs.push((id.clone(), text));
                    }
                    let mut field_begin: Option<String> = None;
                    for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                        if let InlineNode::Decoration(decoration) = inline
                            && decoration.kind == DecorationType::Bookmark
                        {
                            let raw = String::from_utf8_lossy(
                                decoration.raw_xml.as_deref().unwrap_or_default(),
                            );
                            if let Some(name) = raw
                                .split("w:name=\"")
                                .nth(1)
                                .and_then(|rest| rest.split('"').next())
                                && !name.starts_with('_')
                            {
                                host.bookmarks.push((id.clone(), name.to_string()));
                            }
                        }
                        let InlineNode::OpaqueInline(opaque) = inline else {
                            continue;
                        };
                        let opaque_id = opaque.id.to_string();
                        let raw = opaque.raw_xml.as_deref().unwrap_or_default();
                        let raw_str = String::from_utf8_lossy(raw);
                        match &opaque.kind {
                            OpaqueKind::Hyperlink(data) => host.hyperlinks.push((
                                id.clone(),
                                opaque_id,
                                data.text.clone(),
                                data.url.clone(),
                            )),
                            OpaqueKind::Drawing => {
                                if let Some(word) = unique_word(&raw_text(raw)) {
                                    host.opaques_with_text.push((
                                        id.clone(),
                                        opaque_id.clone(),
                                        word,
                                    ));
                                }
                                host.drawings.push(Drawing {
                                    paragraph: id.clone(),
                                    id: opaque_id,
                                    textbox: raw_str.contains("txbxContent"),
                                    anchored: raw_str.contains("wp:anchor"),
                                });
                            }
                            OpaqueKind::Sdt => host.controls.push((
                                id.clone(),
                                opaque_id,
                                raw_str.contains("checkbox"),
                            )),
                            OpaqueKind::Field(data) => {
                                // A legacy form field is addressed by its `begin`.
                                if data.field_kind == FieldKind::Begin {
                                    field_begin = Some(opaque_id);
                                } else if data
                                    .instruction_text
                                    .as_deref()
                                    .is_some_and(|i| i.contains("FORMTEXT"))
                                    && let Some(begin) = field_begin.take()
                                {
                                    host.form_fields.push((id.clone(), begin));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                BlockNode::Table(t) => {
                    let headers = stemma::table::canonicalize_table(t)
                        .map(|grid| {
                            (0..grid.n_cols)
                                .map(|col| {
                                    grid.cell_at(0, col)
                                        .map_or_else(String::new, |c| c.text.clone())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    host.tables.push((t.id.to_string(), headers));
                }
                BlockNode::OpaqueBlock(o) => {
                    if matches!(o.kind, OpaqueKind::Sdt) {
                        host.body_controls.push(index);
                    }
                }
            }
        }
        host.comments = canonical.comments.iter().map(|c| c.id.clone()).collect();
        host.footnotes = canonical
            .footnotes
            .iter()
            .filter(|n| n.id.parse::<i64>().is_ok_and(|id| id > 0))
            .map(|n| n.id.clone())
            .collect();
        let story = |part: &str, blocks: &[TrackedBlock]| {
            blocks.iter().find_map(|tracked| {
                let BlockNode::Paragraph(p) = &tracked.block else {
                    return None;
                };
                let word = unique_word(&text_of(p.segments.iter().flat_map(|s| &s.inlines)))?;
                Some((
                    part.rsplit('/').next().unwrap_or(part).to_string(),
                    p.id.to_string(),
                    word,
                ))
            })
        };
        host.header = canonical
            .headers
            .iter()
            .find_map(|h| story(&h.part_name, &h.blocks));
        host.header_textbox = canonical.headers.iter().find_map(|h| {
            h.blocks.iter().find_map(|tracked| {
                let BlockNode::Paragraph(p) = &tracked.block else {
                    return None;
                };
                p.segments
                    .iter()
                    .flat_map(|s| &s.inlines)
                    .find_map(|inline| {
                        let InlineNode::OpaqueInline(opaque) = inline else {
                            return None;
                        };
                        let raw = opaque.raw_xml.as_deref()?;
                        let word = unique_word(&raw_text(raw))?;
                        (matches!(opaque.kind, OpaqueKind::Drawing)
                            && String::from_utf8_lossy(raw).contains("txbxContent"))
                        .then(|| {
                            (
                                h.part_name
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(&h.part_name)
                                    .to_string(),
                                p.id.to_string(),
                                opaque.id.to_string(),
                                word,
                            )
                        })
                    })
            })
        });
        host.footer = canonical
            .footers
            .iter()
            .find_map(|f| story(&f.part_name, &f.blocks));
        host
    }
}

/// Fill one catalog shape from `host`, or `None` when the host lacks something
/// the shape needs.
fn fill(shape: &str, host: &Host) -> Option<String> {
    let needs = |placeholder: &str| shape.contains(placeholder);
    let mut vars: Vec<(&str, String)> = Vec::new();
    // The object the shape is about decides which paragraph `<block_id>` names.
    let mut object_paragraph: Option<String> = None;

    if needs("<hyperlink_id>") {
        let (p, id, text, url) = host
            .hyperlinks
            .iter()
            .find(|(_, _, text, url)| !text.is_empty() && url.is_some())?;
        let url = url.clone()?;
        vars.extend([
            ("<hyperlink_id>", id.clone()),
            ("<current display text>", text.clone()),
            ("<exact display text>", text.clone()),
            ("<current_url>", url.clone()),
            ("https://example.com/current", url),
        ]);
        object_paragraph = Some(p.clone());
    }
    if needs("<drawing_id>") {
        let textbox = needs("set_textbox_text");
        let anchored = needs("\"wrap\"");
        let drawing = host
            .drawings
            .iter()
            .find(|d| d.textbox == textbox && (!anchored || d.anchored))?;
        vars.push(("<drawing_id>", drawing.id.clone()));
        object_paragraph = Some(drawing.paragraph.clone());
    }
    // A header textbox is addressed by its part and the header paragraph.
    let header_object = needs("<opaque_id>") && needs("header_part");
    if header_object {
        let (part, p, id, word) = host.header_textbox.as_ref()?;
        vars.extend([
            ("header2.xml", part.clone()),
            ("<opaque_id>", id.clone()),
            ("<text currently inside>", word.clone()),
        ]);
        object_paragraph = Some(p.clone());
    } else if needs("<opaque_id>") {
        let (p, id, word) = host.opaques_with_text.first()?;
        vars.extend([
            ("<opaque_id>", id.clone()),
            ("<text currently inside>", word.clone()),
        ]);
        object_paragraph = Some(p.clone());
    }
    if needs("<sdt_id>") {
        let checkbox = needs("\"checked\"");
        let (p, id, _) = host
            .controls
            .iter()
            .find(|(_, _, is_checkbox)| *is_checkbox == checkbox)?;
        vars.push(("<sdt_id>", id.clone()));
        object_paragraph = Some(p.clone());
    }
    if needs("<field_id>") {
        let (p, id) = host.form_fields.first()?;
        vars.push(("<field_id>", id.clone()));
        object_paragraph = Some(p.clone());
    }
    if needs("sdt_text_fill") && !needs("<sdt_id>") && !needs("\"body_index\":0") {
        object_paragraph = Some(host.open_controls.first()?.clone());
    }
    if needs("\"body_index\":0") && needs("sdt_text_fill") {
        vars.push((
            "\"body_index\":0",
            format!("\"body_index\":{}", host.body_controls.first()?),
        ));
    }
    if needs("set_block_form_text") {
        let (body, paragraph, field, hash) = host.block_form_text.as_ref()?;
        vars.extend([
            ("\"body_index\":0", format!("\"body_index\":{body}")),
            (
                "\"paragraph_index\":1",
                format!("\"paragraph_index\":{paragraph}"),
            ),
            ("\"field_index\":0", format!("\"field_index\":{field}")),
            ("<containing-block hash>", hash.clone()),
        ]);
    }
    if needs("<table_id>") {
        let (table, headers) = host.tables.first()?;
        vars.push(("<table_id>", table.clone()));
        if needs("<text of the column's top cell>") {
            // The header of the column the shape names.
            let col: usize = shape
                .split("\"col_index\":")
                .nth(1)?
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()?;
            vars.push((
                "<text of the column's top cell>",
                headers
                    .get(col)?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .replace('\\', "\\\\")
                    .replace('"', "\\\""),
            ));
        }
    }
    if needs("<comment_id>") {
        vars.push(("<comment_id>", host.comments.first()?.clone()));
    }
    if needs("<note_id>") {
        vars.push(("<note_id>", host.footnotes.first()?.clone()));
    }
    if needs("header_part") && !header_object {
        let (part, id, word) = host.header.as_ref()?;
        vars.extend([
            ("header1.xml", part.clone()),
            (
                "<substring currently in the header paragraph>",
                word.clone(),
            ),
        ]);
        object_paragraph = Some(id.clone());
    }
    if needs("footer_part") {
        let (part, id, word) = host.footer.as_ref()?;
        vars.extend([
            ("footer1.xml", part.clone()),
            (
                "<substring currently in the footer paragraph>",
                word.clone(),
            ),
        ]);
        object_paragraph = Some(id.clone());
    }
    // Bookmark operations act on an existing bookmark in its paragraph; a
    // cross-reference points at one.
    let renames = needs("current_bookmark_name");
    let removes = needs("\"op\":\"remove_bookmark\"");
    if renames || removes || needs("insert_cross_ref") {
        let (p, name) = host.bookmarks.first()?;
        vars.push((
            if renames {
                "current_bookmark_name"
            } else {
                "bookmark_name"
            },
            name.clone(),
        ));
        if renames || removes {
            object_paragraph = Some(p.clone());
        }
    }

    // `<block_id>`: the object's paragraph first, then body paragraphs with a
    // unique word, in order of appearance (move/from/to get distinct ones).
    let candidates: Vec<&(String, String)> = host
        .paragraphs
        .iter()
        .filter(|(_, text)| unique_word(text).is_some())
        .collect();
    let mut block_ids: Vec<String> = object_paragraph.iter().cloned().collect();
    block_ids.extend(
        candidates
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| Some(id) != object_paragraph.as_ref()),
    );
    let first = block_ids.first()?;
    let first_text = host
        .paragraphs
        .iter()
        .find(|(id, _)| id == first)
        .map(|(_, text)| text.clone())
        .unwrap_or_default();
    if !needs("header_part") && !needs("footer_part") {
        for key in [
            "<substring currently in the block>",
            "<exact unique text>",
            "<anchor text>",
            "<span text>",
        ] {
            if needs(key) {
                vars.push((key, unique_word(&first_text)?));
            }
        }
    }
    if needs("<text currently in the block>") {
        vars.push((
            "<text currently in the block>",
            first_text.replace('\\', "\\\\").replace('"', "\\\""),
        ));
    }
    if needs("<paragraph_id>") {
        vars.push(("<paragraph_id>", candidates.first()?.0.clone()));
    }
    for key in [
        "<new text before the image>",
        "<new text>",
        "<new display text>",
        "<new value>",
        "<new header text>",
        "<new footer text>",
        "<note body text>",
        "<new note body text>",
        "<comment text>",
        "<reply text>",
        "<description>",
        "<row content, one per column, left-to-right>",
        "<paragraph text, one entry per paragraph>",
        "<cell 1>",
        "<cell 2>",
    ] {
        vars.push((key, NEW_TEXT.to_string()));
    }
    vars.extend([
        ("<new_url>", "https://example.com/laws".to_string()),
        ("<m:oMath fragment>", OMML.to_string()),
    ]);

    let mut filled = shape.to_string();
    for (key, value) in &vars {
        filled = filled.replace(key, value);
    }
    let mut next = 0;
    while let Some(at) = filled.find("<block_id>") {
        filled.replace_range(at..at + "<block_id>".len(), block_ids.get(next)?);
        next += 1;
    }
    // An unfilled placeholder means the host lacks something (the hash-bound
    // form-field address has no host yet).
    (!filled.contains("\"<") || filled.contains("m:oMath")).then_some(filled)
}

// ── The laws ────────────────────────────────────────────────────────────

fn apply(document: &Document, op: &str, mode: &str) -> Result<Document, String> {
    let json = format!(
        r#"{{"ops":[{op}],"materialization_mode":"{mode}","revision":{{"author":"Laws","date":"2026-01-01T00:00:00Z"}}}}"#
    );
    let v4 = parse_transaction(&json).map_err(|err| format!("schema: {err:?}"))?;
    let txn = v4
        .into_edit_transaction()
        .map_err(|err| format!("adapter: {err:?}"))?;
    document
        .apply(&txn)
        .map_err(|err| format!("{:?}: {}", err.code, err.message))
}

fn reopen(document: &Document) -> Result<Document, String> {
    let bytes = document
        .serialize(&ExportOptions::default())
        .map_err(|err| format!("serialize: {:?}: {}", err.code, err.message))?;
    Document::parse(&bytes).map_err(|err| format!("reopen: {err:?}"))
}

/// Same normalization as the IR laws: proofing markers and run boundaries are
/// not content.
fn normalized(document: &CanonDoc) -> CanonDoc {
    fn inlines(list: &mut Vec<InlineNode>) {
        let mut merged: Vec<InlineNode> = Vec::with_capacity(list.len());
        for inline in list.drain(..) {
            match inline {
                InlineNode::Decoration(decoration)
                    if decoration.kind == DecorationType::ProofError => {}
                InlineNode::Text(mut text) => {
                    text.source_run_attrs.clear();
                    if let Some(InlineNode::Text(previous)) = merged.last_mut()
                        && previous.text_role == text.text_role
                        && previous.formatting_change == text.formatting_change
                        && previous.marks == text.marks
                        && previous.style_props == text.style_props
                        && previous.rpr_authored == text.rpr_authored
                    {
                        previous.text.push_str(&text.text);
                    } else {
                        merged.push(InlineNode::Text(text));
                    }
                }
                other => merged.push(other),
            }
        }
        *list = merged;
    }
    fn block(node: &mut BlockNode) {
        match node {
            BlockNode::Paragraph(p) => p.segments.iter_mut().for_each(|s| inlines(&mut s.inlines)),
            BlockNode::Table(t) => t.rows.iter_mut().for_each(|r| {
                r.cells
                    .iter_mut()
                    .for_each(|c| c.blocks.iter_mut().for_each(block))
            }),
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let story =
        |blocks: &mut Vec<TrackedBlock>| blocks.iter_mut().for_each(|t| block(&mut t.block));
    let mut doc = document.clone();
    story(&mut doc.blocks);
    doc.headers.iter_mut().for_each(|s| story(&mut s.blocks));
    doc.footers.iter_mut().for_each(|s| story(&mut s.blocks));
    doc.footnotes.iter_mut().for_each(|s| story(&mut s.blocks));
    doc.endnotes.iter_mut().for_each(|s| story(&mut s.blocks));
    doc.comments.iter_mut().for_each(|s| story(&mut s.blocks));
    // Separator notes are package scaffolding Word writes with a notes part,
    // not content (the domain excludes them from comparison too).
    doc.footnotes
        .retain(|note| note.note_type == NoteType::Normal);
    doc.endnotes
        .retain(|note| note.note_type == NoteType::Normal);
    doc
}

fn differ(left: &Document, right: &Document) -> Option<String> {
    let diffs = compare_canon_docs(
        &normalized(&left.snapshot().canonical),
        &normalized(&right.snapshot().canonical),
    );
    (!diffs.is_empty()).then(|| {
        let shown: Vec<String> = diffs
            .iter()
            .take(2)
            .map(|d| d.to_string().chars().take(300).collect())
            .collect();
        format!("{} difference(s): {}", diffs.len(), shown.join("; "))
    })
}

fn block_id(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => p.id.to_string(),
        BlockNode::Table(t) => t.id.to_string(),
        BlockNode::OpaqueBlock(o) => o.id.to_string(),
    }
}

/// Whether `block` holds a comment or note the operation names.
fn holds_named_annotation(block: &BlockNode, names: &[&str]) -> bool {
    let BlockNode::Paragraph(p) = block else {
        return false;
    };
    p.segments
        .iter()
        .flat_map(|s| &s.inlines)
        .any(|inline| match inline {
            InlineNode::CommentRangeStart { id } | InlineNode::CommentRangeEnd { id } => {
                names.contains(&id.as_str())
            }
            InlineNode::OpaqueInline(o) => match &o.kind {
                OpaqueKind::CommentReference(r)
                | OpaqueKind::FootnoteReference(r)
                | OpaqueKind::EndnoteReference(r) => names.contains(&r.reference_id.as_str()),
                _ => false,
            },
            _ => false,
        })
}

/// Blocks other than the ones the operation names, or that hold what it names,
/// must be unchanged.
fn locality(source: &Document, edited: &Document, op: &str) -> Option<String> {
    let names: Vec<&str> = op.split('"').collect();
    let after: HashMap<String, &TrackedBlock> = edited
        .snapshot()
        .canonical
        .blocks
        .iter()
        .map(|t| (block_id(&t.block), t))
        .collect();
    for tracked in &source.snapshot().canonical.blocks {
        let id = block_id(&tracked.block);
        let named = names
            .iter()
            .any(|name| *name == id || name.starts_with(&format!("{id}_")));
        if named || holds_named_annotation(&tracked.block, &names) {
            continue;
        }
        if let Some(edited_block) = after.get(&id) {
            let diffs = compare_tracked_block_pair(tracked, edited_block);
            if !diffs.is_empty() {
                return Some(format!("untouched block {id} changed: {}", diffs[0]));
            }
        }
    }
    None
}

/// Check L4 for one filled operation on one document. `Err` means the
/// operation does not apply to this host (try another); `Ok` is the verdict.
fn check(source: &Document, op_name: &str, op: &str) -> Result<Vec<String>, String> {
    let direct = apply(source, op, "direct");
    let tracked = apply(source, op, "tracked_change");
    if let (Err(direct_refusal), Err(_)) = (&direct, &tracked) {
        return Err(direct_refusal.clone());
    }
    let mut found = Vec::new();
    let source_saved = reopen(source).map_err(|err| format!("source does not reopen: {err}"))?;
    let spelling = |label: &str, edited: &Document, found: &mut Vec<String>| match reopen(edited) {
        Ok(saved) => {
            if let Some(d) = differ(edited, &saved) {
                found.push(format!("one spelling ({label} vs its saved form): {d}"));
            }
        }
        Err(err) => found.push(format!("{label} does not save: {err}")),
    };

    if let Ok(direct) = &direct {
        spelling("direct edit", direct, &mut found);
        if !is_global(op_name)
            && let Some(violation) = locality(source, direct, op)
        {
            found.push(violation);
        }
    }
    match (&tracked, &direct) {
        (Ok(tracked), _) if tracked.revisions().len() > source.revisions().len() => {
            spelling("tracked edit", tracked, &mut found);
            match tracked
                .project(Resolution::RejectAll)
                .map_err(|err| format!("{err:?}"))
                .and_then(|r| reopen(&r))
            {
                Ok(rejected) => {
                    if let Some(d) = differ(&rejected, &source_saved) {
                        found.push(format!("Reject All does not restore the source: {d}"));
                    }
                }
                Err(err) => found.push(format!("Reject All failed: {err}")),
            }
            if let Ok(direct) = &direct {
                match (
                    tracked
                        .project(Resolution::AcceptAll)
                        .map_err(|err| format!("{err:?}"))
                        .and_then(|r| reopen(&r)),
                    reopen(direct),
                ) {
                    (Ok(accepted), Ok(direct_saved)) => {
                        if let Some(d) = differ(&accepted, &direct_saved) {
                            found.push(format!("Accept All differs from the direct edit: {d}"));
                        }
                    }
                    (Err(err), _) | (_, Err(err)) => {
                        found.push(format!("Accept All failed: {err}"))
                    }
                }
            }
        }
        (Ok(tracked), Ok(direct)) => {
            if let (Ok(t), Ok(d)) = (reopen(tracked), reopen(direct))
                && let Some(diff) = differ(&t, &d)
            {
                found.push(format!(
                    "tracked edit without revisions differs from the direct edit: {diff}"
                ));
            }
        }
        (Ok(_), Err(_)) => {}
        (Err(err), Ok(_)) => {
            if !err.starts_with("UnsupportedEdit") {
                found.push(format!(
                    "tracked form fails where the direct form applies: {err}"
                ));
            }
        }
        (Err(_), Err(_)) => unreachable!("handled above"),
    }
    Ok(found)
}

/// Violations known on 2026-10-25 when this suite entered the gate, as
/// (operation, law). Each is an open engine defect to fix, not an accepted
/// behaviour. The suite fails on any violation not listed here and on any
/// listed one that no longer occurs, so the ledger only shrinks.
const KNOWN_VIOLATIONS: &[(&str, &str)] = &[];

/// Laws Word itself does not satisfy, as (operation, law). These are not
/// defects: the engine does what Word does, confirmed against Microsoft Word.
/// A section property change (`w:sectPrChange`) is rejected RELATIVELY: Word
/// restores only the properties the recorded previous `w:sectPr` lists and
/// keeps every other current one. A source section with no header reference
/// or no page margins has nothing to restore them to, so Reject All keeps a
/// created header or footer reference, and keeps margins set on a section
/// that had none. The suite fails if one stops occurring, like the ledger.
const WORD_SEMANTICS: &[(&str, &str)] = &[
    ("create_footer", "Reject All does not restore the source"),
    ("create_header", "Reject All does not restore the source"),
    ("set_page_setup", "Reject All does not restore the source"),
];

/// The law a violation message names: the text before its first colon.
fn law_of(violation: &str) -> &str {
    let law = violation.split(':').next().unwrap_or(violation);
    if law.starts_with("untouched block") {
        "untouched block changed"
    } else {
        law
    }
}

#[test]
fn every_operation_obeys_the_edit_laws() {
    let documents: Vec<(String, Document)> = fixture_documents()
        .into_iter()
        .filter_map(|(name, bytes)| Some((name, Document::parse(&bytes).ok()?)))
        .collect();
    assert!(
        documents
            .iter()
            .any(|(name, _)| name == "synthetic objects document"),
        "the synthetic objects document must import"
    );
    let hosts: Vec<Host> = documents
        .iter()
        .map(|(_, d)| {
            let mut host = Host::read(&d.snapshot().canonical);
            host.block_form_text = d
                .block_form_field_targets()
                .expect("block form field targets")
                .into_iter()
                .find(|target| target.instruction.contains("FORMTEXT"))
                .map(|target| {
                    (
                        target.body_index,
                        target.paragraph_index,
                        target.field_index,
                        target.semantic_hash,
                    )
                });
            host
        })
        .collect();
    let shapes: Vec<(String, String)> = operation_catalog()
        .into_iter()
        .flat_map(|spec| {
            spec.examples
                .iter()
                .map(move |shape| (spec.name.to_string(), shape.to_string()))
        })
        .collect();

    let failures = std::sync::Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..std::thread::available_parallelism().map_or(1, usize::from) {
            scope.spawn(|| {
                while let Some((name, shape)) =
                    shapes.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    let mut last_refusal = String::from("no document has what the shape needs");
                    let mut hosted = false;
                    for ((document_name, document), host) in documents.iter().zip(&hosts) {
                        let Some(op) = fill(shape, host) else {
                            continue;
                        };
                        match check(document, name, &op) {
                            Ok(violations) => {
                                for violation in violations {
                                    failures.lock().unwrap().push((
                                        name.clone(),
                                        law_of(&violation).to_string(),
                                        format!("{name} on {document_name}: {violation}"),
                                    ));
                                }
                                hosted = true;
                                break;
                            }
                            Err(refusal) => last_refusal = format!("{document_name}: {refusal}"),
                        }
                    }
                    if !hosted {
                        failures.lock().unwrap().push((
                            name.clone(),
                            "unhosted".to_string(),
                            format!("{name}: no document hosts {shape} (last: {last_refusal})"),
                        ));
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    let mut unexpected: Vec<&str> = failures
        .iter()
        .filter(|(op, law, _)| {
            let key = (op.as_str(), law.as_str());
            !KNOWN_VIOLATIONS.contains(&key) && !WORD_SEMANTICS.contains(&key)
        })
        .map(|(_, _, message)| message.as_str())
        .collect();
    unexpected.sort();
    let mut fixed: Vec<String> = KNOWN_VIOLATIONS
        .iter()
        .chain(WORD_SEMANTICS)
        .filter(|(op, law)| !failures.iter().any(|(o, l, _)| o == op && l == law))
        .map(|(op, law)| format!("{op}: {law}"))
        .collect();
    fixed.sort();
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "{} new edit-law violation(s):\n{}\n\n{} ledger entr(ies) no longer occur; remove them from KNOWN_VIOLATIONS or WORD_SEMANTICS:\n{}",
        unexpected.len(),
        unexpected.join("\n"),
        fixed.len(),
        fixed.join("\n")
    );
}
