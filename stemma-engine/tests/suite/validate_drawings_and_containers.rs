//! `validate` and the shapes Word does not open cleanly that were found by
//! asking Word about every document this test suite reads.
//!
//! Each rule was checked against Word with small documents that differ in
//! one piece:
//!
//! - drawings: starting from an inline picture, an anchored picture, an
//!   inline text-box shape and an anchored text-box shape that Word opens
//!   cleanly, one attribute or one child was removed at a time. Word repairs
//!   the document for the removals listed in the rule and opens it cleanly
//!   for the rest. Word cannot open a document whose `w:drawing` is empty or
//!   holds two frames, or whose `a:graphicData` is empty;
//! - a content control that holds one value (plain text, date, drop-down,
//!   combo box, picture) around two paragraphs or a table: Word repairs;
//! - `mc:Ignorable` naming a prefix the root does not declare: Word repairs;
//! - a table cell that is empty or ends with a table: Word repairs. A row
//!   with no cell: Word cannot open the file;
//! - deleted text spelled `w:t`, or other text spelled `w:delText`, in each
//!   nesting of `w:ins` and `w:del`: Word repairs.
//!
//! The same runs showed what Word accepts, and those are tested as sound: a
//! `customXml*Range` start or end with no partner, two drawings sharing an
//! id, an anchor position wrapped in `mc:AlternateContent`, and a comments
//! part under a generic content type when nothing refers to a comment.

use std::io::{Cursor, Write};

use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const NAMESPACES: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#,
);
const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>EXTRA</Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

const PICTURE: &str = concat!(
    r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>"#,
    r#"<pic:nvPicPr><pic:cNvPr id="0" name="Harbour map"/><pic:cNvPicPr/></pic:nvPicPr>"#,
    r#"<pic:blipFill><a:blip r:embed="rId9"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
    r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
    r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>"#,
);
const SHAPE: &str = concat!(
    r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp>"#,
    r#"<wps:cNvSpPr txBox="1"/>"#,
    r#"<wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
    r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr>"#,
    r#"<wps:txbx><w:txbxContent><w:p><w:r><w:t>Gate closes at dusk</w:t></w:r></w:p></w:txbxContent></wps:txbx>"#,
    r#"<wps:bodyPr/></wps:wsp></a:graphicData></a:graphic>"#,
);
const POSITION_V: &str =
    r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#;

fn inline(graphic: &str) -> String {
    format!(
        r#"<w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="320040"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Drawing 1"/><wp:cNvGraphicFramePr/>{graphic}</wp:inline></w:drawing>"#
    )
}

fn anchor(graphic: &str) -> String {
    format!(
        r#"<w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="251658240" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>{POSITION_V}<wp:extent cx="914400" cy="320040"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="1" name="Drawing 1"/><wp:cNvGraphicFramePr/>{graphic}</wp:anchor></w:drawing>"#
    )
}

fn in_run(drawing: &str) -> String {
    format!(
        r#"<w:p><w:r><w:t xml:space="preserve">See the plan </w:t></w:r><w:r>{drawing}</w:r></w:p>"#
    )
}

/// `parts`: (part name, content). `relationships`: (id, kind, target).
/// `overrides`: (part name, content type).
fn package_with(
    root_attributes: &str,
    body: &str,
    parts: &[(&str, &str)],
    relationships: &[(&str, &str, &str)],
    overrides: &[(&str, &str)],
) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NAMESPACES} {root_attributes}><w:body>{body}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#
    );
    let extra: String = overrides
        .iter()
        .map(|(part, content_type)| {
            format!(r#"<Override PartName="/{part}" ContentType="{content_type}"/>"#)
        })
        .collect();
    let content_types = CONTENT_TYPES.replace("EXTRA", &extra);
    let relationships: String = relationships
        .iter()
        .map(|(id, kind, target)| {
            format!(r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{target}"/>"#)
        })
        .collect();
    let document_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{relationships}</Relationships>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let fixed = [
            ("[Content_Types].xml", content_types.as_str()),
            ("_rels/.rels", ROOT_RELS),
            ("word/_rels/document.xml.rels", document_rels.as_str()),
            ("word/document.xml", document.as_str()),
        ];
        for (part, text) in fixed.iter().chain(parts) {
            zip.start_file(*part, opts).expect("start_file");
            zip.write_all(text.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish zip");
    }
    buf
}

fn with_body(body: &str) -> Vec<u8> {
    package_with("", body, &[], &[], &[])
}

fn messages(bytes: &[u8]) -> Vec<String> {
    validate_docx_report(bytes)
        .expect("validate")
        .issues
        .into_iter()
        .map(|issue| issue.message)
        .collect()
}

fn assert_sound(body: &str) {
    assert_eq!(messages(&with_body(body)), Vec::<String>::new(), "{body}");
}

/// Exactly one issue, and it reads `expected`.
fn assert_one_issue(body: &str, expected: &str) {
    assert_eq!(
        messages(&with_body(body)),
        vec![expected.to_string()],
        "{body}"
    );
}

const REPAIRS: &str = "Word reports the file as corrupted and repairs it";
const CANNOT_OPEN: &str = "Word cannot open such a file";

// --- drawings ---------------------------------------------------------------

#[test]
fn complete_drawings_are_sound() {
    for drawing in [
        inline(PICTURE),
        anchor(PICTURE),
        inline(SHAPE),
        anchor(SHAPE),
    ] {
        assert_sound(&in_run(&drawing));
    }
}

#[test]
fn a_drawing_missing_a_required_piece_is_reported() {
    let picture = inline(PICTURE);
    let anchored = anchor(SHAPE);
    let shape = inline(SHAPE);
    let cases: [(&str, &str, &str, &str); 12] = [
        (
            &picture,
            r#"<wp:extent cx="914400" cy="320040"/>"#,
            "",
            "a wp:inline element has no wp:extent child",
        ),
        (
            &picture,
            r#" name="Drawing 1""#,
            "",
            "a wp:docPr element has no name attribute",
        ),
        (
            &picture,
            r#" cy="320040"/><wp:effectExtent"#,
            "/><wp:effectExtent",
            "a wp:extent element has no cy attribute",
        ),
        (
            &picture,
            r#" t="0""#,
            "",
            "a wp:effectExtent element has no t attribute",
        ),
        (
            &picture,
            "<pic:cNvPicPr/>",
            "",
            "a pic:nvPicPr element has no pic:cNvPicPr child",
        ),
        (
            &picture,
            r#"<a:blip r:embed="rId9"/>"#,
            "",
            "a pic:blipFill element has no a:blip child",
        ),
        (
            &picture,
            r#" prst="rect""#,
            "",
            "a a:prstGeom element has no prst attribute",
        ),
        (
            &anchored,
            r#" behindDoc="0""#,
            "",
            "a wp:anchor element has no behindDoc attribute",
        ),
        (
            &anchored,
            "<wp:wrapNone/>",
            "",
            "a wp:anchor element has no wrapping element",
        ),
        (
            &anchored,
            "<wp:posOffset>0</wp:posOffset></wp:positionH>",
            "</wp:positionH>",
            "a wp:positionH element holds no position",
        ),
        (
            &shape,
            "<wps:bodyPr/>",
            "",
            "a wps:wsp element has no wps:bodyPr child",
        ),
        (
            &shape,
            r#"<wps:cNvSpPr txBox="1"/>"#,
            "",
            "a wps:wsp element has neither a wps:cNvSpPr nor a wps:cNvCnPr child",
        ),
    ];
    for (drawing, piece, replacement, problem) in cases {
        assert_eq!(drawing.matches(piece).count(), 1, "{piece}");
        assert_one_issue(
            &in_run(&drawing.replace(piece, replacement)),
            &format!("I-DRAW-001: word/document.xml: {problem}. {REPAIRS}"),
        );
    }
}

#[test]
fn a_shape_without_geometry_is_reported() {
    let geometry = r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr>"#;
    assert_one_issue(
        &in_run(&inline(SHAPE).replace(geometry, "</wps:spPr>")),
        &format!("I-DRAW-001: word/document.xml: a wps:spPr element names no geometry. {REPAIRS}"),
    );
}

/// Pieces Word does not require: the picture opens cleanly without them.
#[test]
fn optional_drawing_pieces_may_be_absent() {
    let picture = inline(PICTURE);
    for piece in [
        r#" distT="0""#,
        r#"<wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
        "<wp:cNvGraphicFramePr/>",
        r#" r:embed="rId9""#,
        "<a:stretch><a:fillRect/></a:stretch>",
        r#"<a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
        r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#,
    ] {
        assert_eq!(picture.matches(piece).count(), 1, "{piece}");
        assert_sound(&in_run(&picture.replace(piece, "")));
    }
}

/// Word writes a percentage position inside `mc:AlternateContent`; the
/// anchor still has its position.
#[test]
fn an_anchor_position_inside_alternate_content_counts() {
    let wrapped = format!(
        r#"<mc:AlternateContent><mc:Choice Requires="wp14">{POSITION_V}</mc:Choice><mc:Fallback>{POSITION_V}</mc:Fallback></mc:AlternateContent>"#
    );
    assert_sound(&in_run(&anchor(PICTURE).replace(POSITION_V, &wrapped)));
}

#[test]
fn a_drawing_id_outside_32_bits_is_reported_and_a_repeated_id_is_sound() {
    for id in ["-1", "4294967296"] {
        assert_one_issue(
            &in_run(
                &inline(PICTURE).replace(r#"<wp:docPr id="1""#, &format!(r#"<wp:docPr id="{id}""#)),
            ),
            &format!(
                r#"I-DRAW-001: word/document.xml: a wp:docPr id "{id}" is not an unsigned 32-bit number. {REPAIRS}"#
            ),
        );
    }
    assert_sound(&format!(
        "{}{}",
        in_run(&inline(PICTURE)),
        in_run(&inline(PICTURE))
    ));
    assert_sound(&in_run(
        &inline(PICTURE).replace(r#"<wp:docPr id="1""#, r#"<wp:docPr id="0""#),
    ));
}

#[test]
fn a_drawing_word_cannot_open_is_reported() {
    let frame = inline(PICTURE)
        .replace("<w:drawing>", "")
        .replace("</w:drawing>", "");
    let not_one_frame = format!(
        "I-DRAW-002: word/document.xml: a w:drawing does not hold exactly one wp:inline or wp:anchor. {CANNOT_OPEN}"
    );
    assert_one_issue(&in_run("<w:drawing/>"), &not_one_frame);
    assert_one_issue(
        &in_run(&format!("<w:drawing>{frame}{frame}</w:drawing>")),
        &not_one_frame,
    );
    assert_one_issue(
        &in_run(&inline(
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"/></a:graphic>"#,
        )),
        &format!("I-DRAW-002: word/document.xml: an a:graphicData element is empty. {CANNOT_OPEN}"),
    );
}

/// Word opens a bare `wps:wsp` cleanly when the graphic data declares
/// another kind, so a shape is judged only in shape graphic data.
#[test]
fn a_shape_under_another_declared_kind_is_not_judged() {
    let bare = r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>North quay</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic>"#;
    assert_sound(&in_run(&inline(bare)));
}

#[test]
fn a_faulty_drawing_in_a_header_is_reported() {
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr {NAMESPACES}>{}</w:hdr>"#,
        in_run(&anchor(SHAPE).replace("<wps:bodyPr/>", ""))
    );
    let bytes = package_with(
        "",
        r#"<w:p><w:r><w:t>Timetable follows.</w:t></w:r></w:p>"#,
        &[("word/header1.xml", &header)],
        &[("rId7", "header", "header1.xml")],
        &[(
            "word/header1.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
        )],
    );
    assert_eq!(
        messages(&bytes),
        vec![format!(
            "I-DRAW-001: word/header1.xml: a wps:wsp element has no wps:bodyPr child. {REPAIRS}"
        )]
    );
}

// --- content controls -------------------------------------------------------

const ONE: &str = r#"<w:p><w:r><w:t>Ferry departs at nine.</w:t></w:r></w:p>"#;
const TWO: &str = r#"<w:p><w:r><w:t>Return sailing at four.</w:t></w:r></w:p>"#;
const TABLE: &str = r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Pier</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;

fn control(kind: &str, content: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:tag w:val="sailing"/><w:id w:val="171561674"/>{kind}</w:sdtPr><w:sdtContent>{content}</w:sdtContent></w:sdt>"#
    )
}

#[test]
fn a_one_value_control_around_more_than_one_paragraph_is_reported() {
    for kind in ["text", "date", "dropDownList", "comboBox", "picture"] {
        assert_one_issue(
            &control(&format!("<w:{kind}/>"), &format!("{ONE}{TWO}")),
            &format!(
                "I-SDT-001: word/document.xml: a w:{kind} content control holds 2 paragraph(s) and 0 table(s); such a control holds exactly one paragraph. {REPAIRS}"
            ),
        );
    }
    assert_one_issue(
        &control("<w:text/>", TABLE),
        &format!(
            "I-SDT-001: word/document.xml: a w:text content control holds 0 paragraph(s) and 1 table(s); such a control holds exactly one paragraph. {REPAIRS}"
        ),
    );
}

#[test]
fn controls_word_opens_cleanly_are_sound() {
    assert_sound(&control("<w:text/>", ONE));
    assert_sound(&control("", &format!("{ONE}{TWO}")));
    let inline_control = control(
        "<w:text/>",
        "<w:r><w:t>nine</w:t></w:r><w:r><w:t> sharp</w:t></w:r>",
    );
    assert_sound(&format!("<w:p>{inline_control}</w:p>"));
}

// --- mc:Ignorable -----------------------------------------------------------

#[test]
fn ignorable_naming_an_undeclared_prefix_is_reported() {
    let bytes = package_with(r#"mc:Ignorable="w14 wp15x""#, ONE, &[], &[], &[]);
    assert_eq!(
        messages(&bytes),
        vec![format!(
            r#"I-MC-001: word/document.xml: mc:Ignorable names the prefix "wp15x", which the root element does not declare. {REPAIRS}"#
        )]
    );
    for declared in [r#"mc:Ignorable="w14""#, r#"mc:Ignorable="""#] {
        let bytes = package_with(declared, ONE, &[], &[], &[]);
        assert_eq!(messages(&bytes), Vec::<String>::new(), "{declared}");
    }
}

// --- table cells and rows ---------------------------------------------------

fn table(cell: &str) -> String {
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr>{cell}</w:tr></w:tbl>"#
    )
}

#[test]
fn a_cell_that_does_not_end_with_a_paragraph_is_reported() {
    let ends_with_table = format!(
        "I-TBL-001: word/document.xml: a table cell ends with a table; a cell ends with a paragraph. {REPAIRS}"
    );
    let holds_nothing = format!(
        "I-TBL-001: word/document.xml: a table cell holds no paragraph; a cell ends with a paragraph. {REPAIRS}"
    );
    assert_one_issue(
        &table(&format!("<w:tc>{ONE}{TABLE}</w:tc>")),
        &ends_with_table,
    );
    assert_one_issue(&table(&format!("<w:tc>{TABLE}</w:tc>")), &ends_with_table);
    assert_one_issue(&table("<w:tc/>"), &holds_nothing);
    assert_one_issue(
        &table(r#"<w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr></w:tc>"#),
        &holds_nothing,
    );
}

#[test]
fn cells_word_opens_cleanly_are_sound() {
    assert_sound(&table(&format!("<w:tc>{TABLE}<w:p/></w:tc>")));
    assert_sound(&table(&format!("<w:tc>{}</w:tc>", control("", ONE))));
    assert_sound(&table(&format!(
        r#"<w:tc><w:bookmarkStart w:id="1" w:name="pier"/>{ONE}<w:bookmarkEnd w:id="1"/></w:tc>"#
    )));
}

#[test]
fn a_row_with_no_cell_is_reported() {
    assert_one_issue(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr/></w:tbl>"#,
        &format!("I-TBL-002: word/document.xml: a table row holds no cell. {CANNOT_OPEN}"),
    );
}

// --- deleted and inserted text ----------------------------------------------

const CHANGE: &str = r#"w:author="Mira" w:date="2026-01-01T00:00:00Z""#;

fn nested(outer: &str, inner: &str, text: &str) -> String {
    format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The tide table </w:t></w:r><w:{outer} w:id="2" {CHANGE}><w:{inner} w:id="1" {CHANGE}><w:r><w:{text} xml:space="preserve">was revised </w:{text}></w:r></w:{inner}></w:{outer}></w:p>"#
    )
}

fn wrapped(change: &str, mark_deleted: bool, text: &str) -> String {
    let mark = if mark_deleted {
        format!(r#"<w:pPr><w:rPr><w:del w:id="6" {CHANGE}/></w:rPr></w:pPr>"#)
    } else {
        String::new()
    };
    format!(
        r#"<w:{change} w:id="5" {CHANGE}><w:p>{mark}<w:r><w:{text}>Low water at six.</w:{text}></w:r></w:p></w:{change}>"#
    )
}

fn text_form_issue(place: &str, found: &str, expected: &str) -> String {
    format!(
        "I-TC-005: word/document.xml: a run {place} spells its text as w:{found}; text there is written as {expected}. {REPAIRS}"
    )
}

/// A deletion and an insertion nested either way hold deleted text.
#[test]
fn text_under_a_nested_deletion_is_deleted_text() {
    for (outer, inner) in [("ins", "del"), ("del", "ins")] {
        assert_sound(&nested(outer, inner, "delText"));
        assert_one_issue(
            &nested(outer, inner, "t"),
            &text_form_issue("inside w:del", "t", "w:delText"),
        );
    }
}

/// A `w:del` around a whole paragraph: the text is deleted text, unless the
/// paragraph mark is itself marked deleted. A `w:ins` around a whole
/// paragraph holds plain text.
#[test]
fn text_in_a_paragraph_wrapped_by_a_tracked_change() {
    assert_sound(&wrapped("del", false, "delText"));
    assert_one_issue(
        &wrapped("del", false, "t"),
        &text_form_issue("in a paragraph inside w:del", "t", "w:delText"),
    );
    assert_sound(&wrapped("del", true, "t"));
    assert_one_issue(
        &wrapped("del", true, "delText"),
        &text_form_issue(
            "in a deleted paragraph whose mark is deleted",
            "delText",
            "w:t",
        ),
    );
    assert_sound(&wrapped("ins", false, "t"));
    assert_one_issue(
        &wrapped("ins", false, "delText"),
        &text_form_issue("in a paragraph inside w:ins", "delText", "w:t"),
    );
}

// --- shapes Word accepts ----------------------------------------------------

/// Word opens a document with half of a customXml range pair cleanly, for
/// every range family.
#[test]
fn half_a_custom_xml_range_pair_is_sound() {
    for end in [
        "customXmlInsRangeEnd",
        "customXmlDelRangeEnd",
        "customXmlMoveFromRangeEnd",
        "customXmlMoveToRangeEnd",
    ] {
        assert_sound(&format!(
            r#"<w:p><w:r><w:t>Kept as written.</w:t></w:r><w:{end} w:id="9"/></w:p>"#
        ));
    }
    for start in ["customXmlInsRangeStart", "customXmlDelRangeStart"] {
        assert_sound(&format!(
            r#"<w:p><w:{start} w:id="9" {CHANGE}/><w:r><w:t>Kept as written.</w:t></w:r></w:p>"#
        ));
    }
}

/// Word reads the comments part only when the document refers to a comment.
#[test]
fn a_comments_part_under_a_generic_type_matters_only_when_referenced() {
    let comments = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="1" w:author="Mira" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:t>Check the tide times.</w:t></w:r></w:p></w:comment></w:comments>"#;
    let parts = [("word/comments.xml", comments)];
    let relationships = [("rId10", "comments", "comments.xml")];
    let unreferenced = package_with("", ONE, &parts, &relationships, &[]);
    assert_eq!(messages(&unreferenced), Vec::<String>::new());

    let referenced_body = r#"<w:p><w:commentRangeStart w:id="1"/><w:r><w:t>Ferry departs at nine.</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r></w:p>"#;
    let referenced = package_with("", referenced_body, &parts, &relationships, &[]);
    assert_eq!(
        messages(&referenced),
        vec![
            r#"comments part "word/comments.xml" is declared with content type "application/xml", not "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"; Word repairs such a file"#
                .to_string()
        ]
    );
}
