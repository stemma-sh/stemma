//! Unique drawing ids.
//!
//! A drawing's non-visual properties (`wp:docPr`, ISO 29500-1 §20.4.2.5)
//! carry an `id` that "shall be unique" within the document. A producer that
//! combines content of two documents (a comparison's deleted copy of a
//! drawing beside its inserted copy, or two documents' drawings that happen
//! to share ids) makes them collide. [`make_drawing_ids_unique`] gives each
//! drawing in inserted content whose id another drawing already uses an
//! unused id, keeping its content hash current. Drawings outside inserted
//! content keep their ids: they are the base document's, and a repeat among
//! them is the base's own (Word opens such a package without repair).
//!
//! Uniqueness is checked on each id as written (string equality), counting
//! every drawing: an id that is missing, empty or not an unsigned int is
//! still the input's own and is kept unless an inserted drawing's written id
//! repeats one already seen. A fresh id is numeric (`ST_DrawingElementId` is
//! an `xsd:unsignedInt`, ISO 29500-1 §20.4.3.6): the largest unsigned int
//! any drawing's id reads as, plus one, or, when that would leave the range,
//! the smallest one no drawing's id reads as.

use std::collections::HashSet;

use crate::domain::{
    BlockNode, CanonDoc, InlineNode, OpaqueInlineNode, TrackedBlock, TrackingStatus,
};

/// Give every drawing in inserted content whose written id another drawing
/// already uses a fresh id (see the module documentation). Body first, then
/// headers, footers, footnotes, endnotes and comments, in their order.
/// Errors: a drawing whose XML does not parse (its ids cannot be checked),
/// and a collision when every unsigned int is taken.
#[doc(hidden)]
pub fn make_drawing_ids_unique(doc: &mut CanonDoc) -> Result<(), String> {
    // First pass: the unsigned ints every id reads as, and the written ids
    // of drawings outside inserted content, which keep theirs.
    let mut numeric: HashSet<u32> = HashSet::new();
    let mut seen: HashSet<String> = HashSet::new();
    for_each_opaque(doc, &mut |opaque, inserted| {
        if let Some(raw) = &opaque.raw_xml
            && has_doc_pr(raw)
        {
            let root = crate::word_xml::parse_raw_fragment(raw)
                .map_err(|e| format!("drawing {} XML: {e}", opaque.id.0))?;
            let mut ids = Vec::new();
            collect_ids(&root, &mut ids);
            numeric.extend(ids.iter().filter_map(|id| as_unsigned_int(id)));
            if !inserted {
                seen.extend(ids);
            }
        }
        Ok(())
    })?;
    for_each_opaque(doc, &mut |opaque, inserted| {
        if !inserted {
            return Ok(());
        }
        let Some(raw) = &opaque.raw_xml else {
            return Ok(());
        };
        if !has_doc_pr(raw) {
            return Ok(());
        }
        let mut root = crate::word_xml::parse_raw_fragment(raw)
            .map_err(|e| format!("drawing {} XML: {e}", opaque.id.0))?;
        if renumber(&mut root, opaque.id.0.as_ref(), &mut seen, &mut numeric)? {
            let rewritten = crate::word_xml::serialize_raw_fragment(&root);
            opaque.content_hash = crate::import::opaque_content_hash_of(&opaque.kind, &rewritten);
            opaque.raw_xml = Some(rewritten);
        }
        Ok(())
    })
}

fn has_doc_pr(raw: &[u8]) -> bool {
    raw.windows(5).any(|w| w == b"docPr")
}

fn is_doc_pr(element: &xmltree::Element) -> bool {
    element.name == "docPr"
        && element.namespace.as_deref()
            == Some("http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing")
}

/// The id of a `wp:docPr` element as written; `None` when it has none.
fn id_of(element: &xmltree::Element) -> Option<&String> {
    element
        .attributes
        .iter()
        .find(|(name, _)| name.local_name == "id" && name.namespace.is_none())
        .map(|(_, value)| value)
}

/// The unsigned int a written id reads as (`xsd:unsignedInt`, whitespace
/// collapsed); `None` when it is not one.
fn as_unsigned_int(id: &str) -> Option<u32> {
    id.trim().parse().ok()
}

/// Every written `wp:docPr` id under `element`, in document order.
fn collect_ids(element: &xmltree::Element, out: &mut Vec<String>) {
    if is_doc_pr(element)
        && let Some(id) = id_of(element)
    {
        out.push(id.clone());
    }
    for child in &element.children {
        if let xmltree::XMLNode::Element(child) = child {
            collect_ids(child, out);
        }
    }
}

/// An unsigned int no drawing's id reads as: the largest plus one, or the
/// smallest unused one when that would leave the range.
fn fresh_id(numeric: &HashSet<u32>) -> Option<u32> {
    match numeric.iter().max() {
        None => Some(1),
        Some(max) => max
            .checked_add(1)
            .or_else(|| (1..=u32::MAX).find(|id| !numeric.contains(id))),
    }
}

/// Renumber the `wp:docPr` ids under `element` whose written id is already
/// `seen`, each to a fresh id; returns whether any changed.
fn renumber(
    element: &mut xmltree::Element,
    drawing: &str,
    seen: &mut HashSet<String>,
    numeric: &mut HashSet<u32>,
) -> Result<bool, String> {
    let mut changed = false;
    if is_doc_pr(element)
        && let Some(id) = id_of(element).cloned()
        && !seen.insert(id.clone())
    {
        let fresh = fresh_id(numeric).ok_or_else(|| {
            format!("drawing {drawing}: wp:docPr id {id:?} is in use and no unused id is left")
        })?;
        numeric.insert(fresh);
        seen.insert(fresh.to_string());
        for (name, value) in element.attributes.iter_mut() {
            if name.local_name == "id" && name.namespace.is_none() {
                *value = fresh.to_string();
            }
        }
        changed = true;
    }
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            changed |= renumber(child, drawing, seen, numeric)?;
        }
    }
    Ok(changed)
}

/// Every inline opaque of the document, body first, then each story, with
/// whether it lies in inserted content (its segment, block, row or cell).
fn for_each_opaque(
    doc: &mut CanonDoc,
    f: &mut dyn FnMut(&mut OpaqueInlineNode, bool) -> Result<(), String>,
) -> Result<(), String> {
    type Visit<'a> = dyn FnMut(&mut OpaqueInlineNode, bool) -> Result<(), String> + 'a;
    fn inserted(status: &TrackingStatus) -> bool {
        matches!(status, TrackingStatus::Inserted(_))
    }
    fn blocks(blocks: &mut [TrackedBlock], f: &mut Visit<'_>) -> Result<(), String> {
        for tracked in blocks {
            let within = inserted(&tracked.status);
            block(&mut tracked.block, within, f)?;
        }
        Ok(())
    }
    fn block(node: &mut BlockNode, within: bool, f: &mut Visit<'_>) -> Result<(), String> {
        match node {
            BlockNode::Paragraph(paragraph) => {
                for segment in &mut paragraph.segments {
                    let within = within || inserted(&segment.status);
                    for inline in &mut segment.inlines {
                        if let InlineNode::OpaqueInline(opaque) = inline {
                            f(opaque, within)?;
                        }
                    }
                }
            }
            BlockNode::Table(table) => {
                for row in &mut table.rows {
                    let within = within || row.tracking_status.as_ref().is_some_and(inserted);
                    for cell in &mut row.cells {
                        let within = within || cell.tracking_status.as_ref().is_some_and(inserted);
                        for inner in &mut cell.blocks {
                            block(inner, within, f)?;
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
        Ok(())
    }
    blocks(&mut doc.blocks, f)?;
    for story in &mut doc.headers {
        blocks(&mut story.blocks, f)?;
    }
    for story in &mut doc.footers {
        blocks(&mut story.blocks, f)?;
    }
    for story in &mut doc.footnotes {
        blocks(&mut story.blocks, f)?;
    }
    for story in &mut doc.endnotes {
        blocks(&mut story.blocks, f)?;
    }
    for story in &mut doc.comments {
        blocks(&mut story.blocks, f)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const NAMESPACES: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;

    /// A run holding an inline drawing whose `wp:docPr` id is `id`.
    fn drawing(id: &str) -> String {
        format!(
            r#"<w:r><w:drawing><wp:inline><wp:extent cx="95250" cy="95250"/><wp:docPr id="{id}" name="Shape {id}"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"/></a:graphic></wp:inline></w:drawing></w:r>"#
        )
    }

    fn inserted(runs: &str) -> String {
        format!(
            r#"<w:ins w:id="90" w:author="Reviewer" w:date="2026-01-05T00:00:00Z">{runs}</w:ins>"#
        )
    }

    /// The canonical document of a body of `paragraphs` (each a run list).
    fn doc(paragraphs: &[String]) -> CanonDoc {
        let body: String = paragraphs
            .iter()
            .map(|p| format!("<w:p>{p}</w:p>"))
            .collect();
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NAMESPACES}><w:body>{body}<w:sectPr/></w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let mut bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::FileOptions::default();
            for (name, part) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", rels),
                ("word/document.xml", document_xml.as_str()),
            ] {
                zip.start_file(name, options).expect("start part");
                zip.write_all(part.as_bytes()).expect("write part");
            }
            zip.finish().expect("finish package");
        }
        let document = crate::api::Document::parse(&bytes).expect("parse");
        (*document.snapshot().canonical).clone()
    }

    /// Each drawing's id as written, in document order, with whether it is
    /// in inserted content.
    fn ids(doc: &mut CanonDoc) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for_each_opaque(doc, &mut |opaque, inserted| {
            if let Some(raw) = &opaque.raw_xml
                && has_doc_pr(raw)
            {
                let root = crate::word_xml::parse_raw_fragment(raw).expect("drawing XML");
                fn walk(element: &xmltree::Element, inserted: bool, out: &mut Vec<(String, bool)>) {
                    if is_doc_pr(element)
                        && let Some(id) = id_of(element)
                    {
                        out.push((id.clone(), inserted));
                    }
                    for child in &element.children {
                        if let xmltree::XMLNode::Element(child) = child {
                            walk(child, inserted, out);
                        }
                    }
                }
                walk(&root, inserted, &mut out);
            }
            Ok(())
        })
        .expect("visit");
        out
    }

    fn written(ids: &[(&str, bool)]) -> Vec<(String, bool)> {
        ids.iter().map(|(id, ins)| (id.to_string(), *ins)).collect()
    }

    #[test]
    fn an_inserted_drawing_repeating_a_kept_id_gets_the_next_unused_id() {
        let mut doc = doc(&[drawing("4"), drawing("7"), inserted(&drawing("4"))]);
        assert_eq!(
            ids(&mut doc),
            written(&[("4", false), ("7", false), ("4", true)])
        );
        make_drawing_ids_unique(&mut doc).expect("unique ids");
        assert_eq!(
            ids(&mut doc),
            written(&[("4", false), ("7", false), ("8", true)])
        );
    }

    #[test]
    fn inserted_drawings_repeating_each_other_are_renumbered_after_the_first() {
        let mut doc = doc(&[inserted(&format!("{}{}", drawing("3"), drawing("3")))]);
        make_drawing_ids_unique(&mut doc).expect("unique ids");
        assert_eq!(ids(&mut doc), written(&[("3", true), ("4", true)]));
    }

    /// The base's own repeat is the base's (Word opens it without repair);
    /// only inserted content is renumbered.
    #[test]
    fn a_repeat_among_kept_drawings_is_left_as_written() {
        let mut doc = doc(&[drawing("2"), drawing("2")]);
        make_drawing_ids_unique(&mut doc).expect("unique ids");
        assert_eq!(ids(&mut doc), written(&[("2", false), ("2", false)]));
    }

    /// The largest unsigned int is a valid id; one past it is not, so the
    /// fresh id is the smallest unused one.
    #[test]
    fn a_fresh_id_past_the_largest_unsigned_int_is_the_smallest_unused_one() {
        let mut doc = doc(&[
            drawing("4294967295"),
            drawing("1"),
            inserted(&drawing("4294967295")),
        ]);
        make_drawing_ids_unique(&mut doc).expect("unique ids");
        assert_eq!(
            ids(&mut doc),
            written(&[("4294967295", false), ("1", false), ("2", true)])
        );
    }

    /// An id that is not an unsigned int is still the input's own: kept
    /// where nothing repeats it, and an inserted drawing repeating it as
    /// written gets a fresh numeric id.
    #[test]
    fn an_id_that_is_not_an_unsigned_int_is_checked_as_written() {
        for odd in ["pic", "18446744073709551615", "-1", ""] {
            let mut repeated = doc(&[drawing(odd), drawing("5"), inserted(&drawing(odd))]);
            make_drawing_ids_unique(&mut repeated).expect(odd);
            assert_eq!(
                ids(&mut repeated),
                written(&[(odd, false), ("5", false), ("6", true)]),
                "{odd:?}"
            );
            let mut alone = doc(&[drawing("5"), inserted(&drawing(odd))]);
            make_drawing_ids_unique(&mut alone).expect(odd);
            assert_eq!(
                ids(&mut alone),
                written(&[("5", false), (odd, true)]),
                "{odd:?} repeats nothing"
            );
        }
    }

    #[test]
    fn a_drawing_without_an_id_is_left_as_written() {
        let no_id = drawing("1").replace(r#" id="1""#, "");
        let mut without = doc(&[no_id.clone(), inserted(&no_id)]);
        make_drawing_ids_unique(&mut without).expect("no ids to repeat");
        assert!(ids(&mut without).is_empty());
    }

    #[test]
    fn a_fresh_id_follows_the_largest_or_fills_the_smallest_gap() {
        assert_eq!(fresh_id(&HashSet::from([u32::MAX])), Some(1));
        assert_eq!(fresh_id(&HashSet::from([u32::MAX, 1, 2])), Some(3));
        assert_eq!(fresh_id(&HashSet::new()), Some(1));
        assert_eq!(fresh_id(&HashSet::from([5, 9])), Some(10));
    }
}
