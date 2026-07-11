//! spec: how an opaque inline object reads in stemma's two distinct text
//! surfaces.
//!
//! There are two surfaces with two contracts, deliberately different:
//!
//!  - **Block-identity surface** (`import::extract_block_text`, the input to the
//!    story content hash): EVERY opaque anchor — drawing, field, footnote
//!    reference, equation — is one Unicode OBJECT REPLACEMENT CHARACTER (U+FFFC,
//!    Unicode 5.4.6). This keeps the opaque inventory countable (one anchor ⇔ one
//!    U+FFFC) and block identity stable against volatile field results (a `PAGE`
//!    field flipping `7`→`8` must not change the block's hash).
//!
//!  - **Human-readable surface** (`view::to_plain_text` / `Document::to_text`):
//!    a field reads as its CACHED RESULT TEXT — the displayed result Word shows —
//!    because a field is not a no-text object to a reader. Every other opaque
//!    object (no textual representation) still reads as one U+FFFC.
//!
//! These tests pin both contracts and the divergence between them on a field.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::import::extract_block_text;
use stemma::view::to_plain_text;

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn block_identity_surface_renders_field_as_object_replacement_character() {
    // §Unicode U+FFFC — on the BLOCK-IDENTITY surface (`extract_block_text`, the
    // hash input) a field is a no-text object: its stream position is exactly one
    // OBJECT REPLACEMENT CHARACTER, never its cached result, so block identity is
    // stable against a volatile field result.
    let body = r#"<w:p><w:r><w:t>See </w:t></w:r><w:fldSimple w:instr=" REF Defs \h "><w:r><w:t>Section 2</w:t></w:r></w:fldSimple><w:r><w:t> now</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let snapshot = doc.snapshot();

    let engine_text = extract_block_text(&snapshot.canonical.blocks[0].block);
    assert_eq!(
        engine_text.matches('\u{FFFC}').count(),
        1,
        "exactly one U+FFFC for one opaque object on the identity surface"
    );
    // The cached result must NOT replace the U+FFFC (would break one-anchor⇔one-
    // FFFC and let the result text destabilize the block hash).
    assert_eq!(engine_text, "See \u{FFFC} now");
}

#[test]
fn human_readable_surface_renders_field_as_its_cached_result() {
    // On the HUMAN-READABLE surface (`to_plain_text`) the same field reads as its
    // cached result "Section 2" — the displayed result Word shows — NOT a U+FFFC.
    let body = r#"<w:p><w:r><w:t>See </w:t></w:r><w:fldSimple w:instr=" REF Defs \h "><w:r><w:t>Section 2</w:t></w:r></w:fldSimple><w:r><w:t> now</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let text = to_plain_text(&doc.read());

    assert_eq!(
        text, "See Section 2 now",
        "the field's cached result reads as text on the human-readable surface: {text:?}"
    );
    assert!(
        !text.contains('\u{FFFC}'),
        "a field with a cached result contributes no U+FFFC to the human-readable surface: {text:?}"
    );
}

#[test]
fn human_readable_and_identity_surfaces_diverge_only_on_fields() {
    // The two surfaces agree on text runs and on no-text objects (a drawing reads
    // as one U+FFFC on both), and diverge ONLY where a field carries a cached
    // result: identity surface = U+FFFC, human-readable surface = the result.
    let body = r#"<w:p><w:r><w:t>Alpha </w:t></w:r><w:fldSimple w:instr=" REF X \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t> mid </w:t></w:r><w:r><w:drawing/></w:r><w:r><w:t> omega</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let snapshot = doc.snapshot();
    let view = doc.read();
    assert_eq!(view.blocks.len(), snapshot.canonical.blocks.len());

    let identity = extract_block_text(&snapshot.canonical.blocks[0].block);
    let readable = stemma::view::to_plain_text_blocks(std::slice::from_ref(&view.blocks[0]));

    // Identity surface: field + drawing are both U+FFFC.
    assert_eq!(identity, "Alpha \u{FFFC} mid \u{FFFC} omega");
    // Human-readable surface: field reads as "X", drawing stays U+FFFC.
    assert_eq!(readable, "Alpha X mid \u{FFFC} omega");
}
