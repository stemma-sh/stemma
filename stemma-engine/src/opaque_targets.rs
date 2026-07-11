//! Opaque-interior discovery — the read-side enumeration of editable text inside
//! opaque regions (a textbox's `w:txbxContent`, an inline content control's
//! `w:sdtContent`), plus the single traversal that both this discovery and the
//! audit revision census share so they can never disagree on WHICH opaques carry
//! an interior.
//!
//! RFC-0002 decision #1: discovery MUST share the census's opaque-interior walk
//! rather than add a parallel traversal — two walks would drift (the audit
//! lesson that produced the census/resolvable split). [`visit_opaque_interiors`]
//! is that one walk; `tracked_model::enumerate_revisions` (the census) and
//! [`opaque_text_targets`] (discovery) are its two consumers. The walk decides
//! which opaque nodes exist and where; each consumer filters for its own purpose
//! (the census: any inline with tracked markup + quarantined blocks; discovery:
//! textbox/SDT text). The reach — body, every story, and recursively through
//! table cells — is defined here, once.
//!
//! Addressing is by the opaque node's own [`NodeId`] plus an [`InteriorAddress`]
//! (container + paragraph index) the edit verb re-navigates against a fresh parse
//! of the fragment. Block-guard hashing catches structural drift between a
//! discovered address and a later edit.

use xmltree::{Element, XMLNode};

use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueBlockNode, OpaqueInlineNode, OpaqueKind,
    StoryScope, TrackedBlock,
};

// ─── The single shared opaque-interior traversal ────────────────────────────

/// One opaque interior yielded by [`visit_opaque_interiors`]: either an inline
/// widget (drawing/pict/object/inline-SDT — carries its bytes in `raw_xml`) or a
/// body-level block opaque (whose bytes live in the serialize scaffold, not on
/// the node, so there is no `raw_xml` to read at the `CanonDoc` level).
pub(crate) enum OpaqueInteriorRef<'a> {
    Inline(&'a OpaqueInlineNode),
    Block(&'a OpaqueBlockNode),
}

/// Visit every opaque node in the document — body, every header/footer/foot-
/// note/endnote/comment story, and recursively through table cells — in document
/// order. This is the canonical reach both the revision census and text-target
/// discovery consume (RFC-0002 §decision-1). `host_block_id` is the enclosing
/// paragraph's id for an inline opaque, or the block's own id for a block opaque.
pub(crate) fn visit_opaque_interiors<F>(doc: &CanonDoc, f: &mut F)
where
    F: FnMut(&NodeId, &StoryScope, OpaqueInteriorRef<'_>),
{
    visit_blocks(&doc.blocks, &StoryScope::Body, f);
    for s in &doc.headers {
        visit_blocks(
            &s.blocks,
            &StoryScope::Header {
                part_path: s.part_name.clone(),
                kind: s.kind.clone(),
            },
            f,
        );
    }
    for s in &doc.footers {
        visit_blocks(
            &s.blocks,
            &StoryScope::Footer {
                part_path: s.part_name.clone(),
                kind: s.kind.clone(),
            },
            f,
        );
    }
    for s in &doc.footnotes {
        visit_blocks(&s.blocks, &StoryScope::Footnote { id: s.id.clone() }, f);
    }
    for s in &doc.endnotes {
        visit_blocks(&s.blocks, &StoryScope::Endnote { id: s.id.clone() }, f);
    }
    for s in &doc.comments {
        visit_blocks(&s.blocks, &StoryScope::Comment { id: s.id.clone() }, f);
    }
}

fn visit_blocks<F>(blocks: &[TrackedBlock], location: &StoryScope, f: &mut F)
where
    F: FnMut(&NodeId, &StoryScope, OpaqueInteriorRef<'_>),
{
    for tb in blocks {
        visit_block(&tb.block, location, f);
    }
}

fn visit_block<F>(block: &BlockNode, location: &StoryScope, f: &mut F)
where
    F: FnMut(&NodeId, &StoryScope, OpaqueInteriorRef<'_>),
{
    match block {
        BlockNode::Paragraph(p) => {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline {
                    f(&p.id, location, OpaqueInteriorRef::Inline(o));
                }
            }
        }
        BlockNode::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for nested in &cell.blocks {
                        visit_block(nested, location, f);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(o) => {
            f(&o.id, location, OpaqueInteriorRef::Block(o));
        }
    }
}

// ─── Text-target discovery ──────────────────────────────────────────────────

/// What kind of editable text region a target names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpaqueTextTargetKind {
    /// A text-bearing `w:p` inside a textbox's `w:txbxContent`.
    TextboxParagraph,
    /// The text region of an inline content control's `w:sdtContent`.
    InlineSdtText,
}

/// Stable address of an editable text region inside an opaque fragment. The edit
/// verb re-parses the opaque's `raw_xml` and navigates by these indices, so the
/// address stays valid as long as the fragment's text structure does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteriorAddress {
    /// Which container within the fragment: the Nth DISTINCT (text-deduped)
    /// `w:txbxContent`, or the Nth `w:sdtContent`, in document order. Textbox
    /// Choice/Fallback copies share one container index (they are one logical
    /// interior the edit mirrors across).
    pub container_index: usize,
    /// For a textbox, the 0-based index of the text-bearing `w:p` within that
    /// container. For an inline SDT text region, always 0 (the region is one
    /// addressable unit).
    pub paragraph_index: usize,
}

/// A discovered editable text region inside an opaque node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueTextTarget {
    /// The story the hosting opaque lives in.
    pub location: StoryScope,
    /// The enclosing paragraph's id (the block that hosts the opaque inline).
    pub host_block_id: NodeId,
    /// The opaque node's own id — the handle the edit verb addresses.
    pub opaque_id: NodeId,
    pub kind: OpaqueTextTargetKind,
    pub address: InteriorAddress,
    /// The region's current visible `w:t` text. NOTE: includes text inside
    /// PENDING tracked changes (a `w:ins` reads as its inserted text) — the
    /// as-shown view. When `has_tracked_changes` is set, an edit of this
    /// region refuses (`RegionHasTrackedChanges`) until those are resolved.
    pub text: String,
    /// The region already carries tracked-change markup in its editable text
    /// (`opaque_splice::region_has_tracked_containers` — the same predicate
    /// the edit verb refuses on). Surfaced so a caller knows the text is
    /// readable but not editable until the pending changes are resolved,
    /// instead of discovering it via the refusal.
    pub has_tracked_changes: bool,
}

/// Enumerate every editable interior text region reachable at the `CanonDoc`
/// level: textbox paragraphs and inline-SDT text regions. Body-level (block) SDTs
/// carry no `raw_xml` here — their discovery/fill is a runtime concern that needs
/// the serialize scaffold and is handled at that layer.
pub fn opaque_text_targets(doc: &CanonDoc) -> Vec<OpaqueTextTarget> {
    let mut out = Vec::new();
    visit_opaque_interiors(doc, &mut |host, location, interior| {
        if let OpaqueInteriorRef::Inline(o) = interior {
            collect_inline_targets(o, host, location, &mut out);
        }
    });
    out
}

/// A fillable body-level (block) content control, discovered from the serialize
/// scaffold (its bytes are NOT on the IR node). Addressed for `sdt_text_fill` by
/// the frozen `body_index`. RFC-0002 §Phase-2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSdtTextTarget {
    /// The body index of the block-level `w:sdt` (the `sdt_text_fill` address).
    pub body_index: usize,
    /// The control's `w:sdtPr/w:tag` value, if any.
    pub tag: Option<String>,
    /// The control's `w:sdtPr/w:alias` value, if any.
    pub alias: Option<String>,
    /// The control's current visible text.
    pub text: String,
}

/// Project a body-level opaque child node into a block content-control target, if
/// it is a `w:sdt` carrying text. `None` for non-SDT opaque children (body-level
/// bookmark markers, quarantined blocks) or an empty control.
pub(crate) fn block_sdt_target(body_index: usize, node: &XMLNode) -> Option<BlockSdtTextTarget> {
    let XMLNode::Element(el) = node else {
        return None;
    };
    if !is_w(el, "sdt") {
        return None;
    }
    let content = child_by_local(el, "sdtContent")?;
    // The fill sets the FIRST text-bearing paragraph; discover exactly that, and
    // only if it is cleanly fillable (all text in direct simple runs). A control
    // whose value hides in a hyperlink/field is not advertised (would relocate
    // that text on set) — matching the inline path and the `set_region_text` guard.
    let para = first_text_paragraph(content)?;
    let text = crate::opaque_splice::fillable_text(para)?;
    if text.is_empty() {
        return None;
    }
    let sdt_pr = child_by_local(el, "sdtPr");
    let val_of = |local: &str| -> Option<String> {
        sdt_pr
            .and_then(|pr| child_by_local(pr, local))
            .and_then(|e| crate::xml_attrs::attr_get(e, "w:val").cloned())
    };
    Some(BlockSdtTextTarget {
        body_index,
        tag: val_of("tag"),
        alias: val_of("alias"),
        text,
    })
}

fn child_by_local<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|c| match c {
        XMLNode::Element(e) if is_w(e, local) => Some(e),
        _ => None,
    })
}

/// The first `w:p` in `root`'s subtree (self included) carrying any `w:t` text —
/// the immutable twin of `opaque_splice::first_text_paragraph_mut`, so block-SDT
/// discovery inspects exactly the paragraph the fill will set.
fn first_text_paragraph(root: &Element) -> Option<&Element> {
    if is_w(root, "p") {
        let mut t = String::new();
        collect_wt_text(root, &mut t);
        if !t.is_empty() {
            return Some(root);
        }
    }
    root.children.iter().find_map(|c| match c {
        XMLNode::Element(e) => first_text_paragraph(e),
        _ => None,
    })
}

fn collect_inline_targets(
    o: &OpaqueInlineNode,
    host: &NodeId,
    location: &StoryScope,
    out: &mut Vec<OpaqueTextTarget>,
) {
    // Discovery is read-only enumeration: an opaque with no bytes, or bytes we
    // cannot parse, simply exposes no editable region (the edit verb would refuse
    // it anyway). This is not a silent fallback — there is genuinely nothing
    // addressable, and the census reports any hidden revisions separately.
    let Some(raw) = &o.raw_xml else { return };
    let Ok(root) = crate::word_xml::parse_raw_fragment(raw) else {
        return;
    };
    match &o.kind {
        // Textboxes import as `Drawing` whether DrawingML (`wps:txbx`) or VML
        // (`w:pict` → `v:textbox`); both carry a `w:txbxContent` story.
        OpaqueKind::Drawing => collect_textbox_targets(&root, o, host, location, out),
        OpaqueKind::Sdt => collect_inline_sdt_target(&root, o, host, location, out),
        _ => {}
    }
}

fn collect_textbox_targets(
    root: &Element,
    o: &OpaqueInlineNode,
    host: &NodeId,
    location: &StoryScope,
    out: &mut Vec<OpaqueTextTarget>,
) {
    let mut contents = Vec::new();
    crate::opaque_meta::collect_descendants_by_local(root, "txbxContent", &mut contents);
    // Dedupe Choice/Fallback copies by their VISIBLE-TEXT signature (not bytes):
    // copies whose paragraph texts agree are one logical interior the edit
    // mirrors across, so they share one container index (matching
    // `opaque_meta::textbox_interior_text` and the edit verb's mirror match).
    // Copies with DIVERGENT text get separate container indices — separately
    // addressable, never silently merged.
    let mut seen: Vec<String> = Vec::new();
    for content in contents {
        let paragraphs = textbox_paragraph_texts(content);
        if paragraphs.is_empty() {
            continue;
        }
        let signature = paragraphs.join("\n");
        if seen.contains(&signature) {
            continue;
        }
        let container_index = seen.len();
        seen.push(signature);
        let tracked_flags = textbox_paragraph_tracked_flags(content);
        for (paragraph_index, text) in paragraphs.into_iter().enumerate() {
            out.push(OpaqueTextTarget {
                location: location.clone(),
                host_block_id: host.clone(),
                opaque_id: o.id.clone(),
                kind: OpaqueTextTargetKind::TextboxParagraph,
                address: InteriorAddress {
                    container_index,
                    paragraph_index,
                },
                text,
                has_tracked_changes: tracked_flags.get(paragraph_index).copied().unwrap_or(false),
            });
        }
    }
}

/// Per addressable textbox paragraph (same predicate/order as
/// `textbox_paragraph_texts`): does its editable text already carry
/// tracked-change markup? One flag per surfaced paragraph, index-aligned.
fn textbox_paragraph_tracked_flags(content: &Element) -> Vec<bool> {
    let mut out = Vec::new();
    for child in &content.children {
        if let XMLNode::Element(el) = child
            && is_w(el, "p")
        {
            let mut text = String::new();
            collect_wt_text(el, &mut text);
            if !text.is_empty() {
                out.push(crate::opaque_splice::region_has_tracked_containers(el));
            }
        }
    }
    out
}

fn collect_inline_sdt_target(
    root: &Element,
    o: &OpaqueInlineNode,
    host: &NodeId,
    location: &StoryScope,
    out: &mut Vec<OpaqueTextTarget>,
) {
    let mut contents = Vec::new();
    crate::opaque_meta::collect_descendants_by_local(root, "sdtContent", &mut contents);
    // The outermost `w:sdtContent` is the control's value region; nested SDTs are
    // out of v1 scope (refused loudly at edit time, not surfaced as a target).
    let Some(content) = contents.first() else {
        return;
    };
    // Surface all text-bearing inline controls: `opaque_text_edit` can edit their
    // text (descending through a hyperlink/smart tag), and `sdt_text_fill` refuses
    // loud on the non-fillable ones (the `set_region_text` guard). Discovery is
    // "here is editable text", not "here is a fillable value".
    let mut text = String::new();
    collect_wt_text(content, &mut text);
    if text.is_empty() {
        return;
    }
    out.push(OpaqueTextTarget {
        location: location.clone(),
        host_block_id: host.clone(),
        opaque_id: o.id.clone(),
        kind: OpaqueTextTargetKind::InlineSdtText,
        address: InteriorAddress {
            container_index: 0,
            paragraph_index: 0,
        },
        text,
        has_tracked_changes: crate::opaque_splice::region_has_tracked_containers(content),
    });
}

// ─── Interior navigation shared with the edit verb ──────────────────────────

/// The `w:t` text of every DIRECT text-bearing `w:p` child of a `w:txbxContent`,
/// in order. The predicate "has any `w:t` text" is the one both discovery and the
/// edit verb use to index textbox paragraphs, so an address minted here resolves
/// to the same paragraph at edit time. Nested tables inside a textbox are out of
/// v1 scope — only direct paragraph children are addressable.
pub(crate) fn textbox_paragraph_texts(content: &Element) -> Vec<String> {
    let mut out = Vec::new();
    for child in &content.children {
        if let XMLNode::Element(el) = child
            && is_w(el, "p")
        {
            let mut text = String::new();
            collect_wt_text(el, &mut text);
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

fn is_w(e: &Element, local: &str) -> bool {
    crate::word_xml::is_w_tag(e, local)
}

/// Concatenate the `w:t` (visible, non-deleted) text under `element`. Deleted
/// text (`w:delText`) is deliberately excluded — it is already a tracked deletion,
/// not editable current content.
pub(crate) fn collect_wt_text(element: &Element, out: &mut String) {
    if is_w(element, "t") {
        for child in &element.children {
            if let XMLNode::Text(t) | XMLNode::CData(t) = child {
                out.push_str(t);
            }
        }
        return;
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_wt_text(el, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CanonDoc;
    use crate::runtime::{DocxRuntime, SimpleRuntime};

    /// Wrap a `w:body` inner fragment in a minimal, valid OPC package and import
    /// it to a `CanonDoc`. Corpus-free — built in-process so the daily gate needs
    /// no fixtures.
    fn import_body(body_inner: &str) -> std::sync::Arc<CanonDoc> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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
        SimpleRuntime::new()
            .import_docx(&buf)
            .expect("import synthetic docx")
            .canonical
    }

    /// A `w:p` hosting an inline DrawingML textbox whose interior is `paras`
    /// (each a plain-text paragraph).
    fn textbox_paragraph(paras: &[&str]) -> String {
        let inner: String = paras
            .iter()
            .map(|t| format!(r#"<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"#))
            .collect();
        format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        )
    }

    #[test]
    fn discovers_textbox_paragraphs_in_order() {
        let doc = import_body(&textbox_paragraph(&["First line", "Second line"]));
        let targets = opaque_text_targets(&doc);
        assert_eq!(targets.len(), 2, "one target per text-bearing textbox ¶");
        assert!(
            targets
                .iter()
                .all(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
        );
        assert_eq!(targets[0].text, "First line");
        assert_eq!(targets[0].address.paragraph_index, 0);
        assert_eq!(targets[1].text, "Second line");
        assert_eq!(targets[1].address.paragraph_index, 1);
        // Both paragraphs live in the same textbox → same opaque id & container.
        assert_eq!(targets[0].opaque_id, targets[1].opaque_id);
        assert_eq!(targets[0].address.container_index, 0);
        assert_eq!(targets[1].address.container_index, 0);
    }

    #[test]
    fn discovers_inline_sdt_text() {
        let body = r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="Tenant"/><w:tag w:val="tenant"/></w:sdtPr><w:sdtContent><w:r><w:t>Acme Corp</w:t></w:r></w:sdtContent></w:sdt></w:p>"#;
        let doc = import_body(body);
        let targets = opaque_text_targets(&doc);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, OpaqueTextTargetKind::InlineSdtText);
        assert_eq!(targets[0].text, "Acme Corp");
    }

    #[test]
    fn empty_textbox_yields_no_targets() {
        let doc = import_body(&textbox_paragraph(&[]));
        assert!(opaque_text_targets(&doc).is_empty());
    }

    /// The anti-drift guarantee (RFC-0002 §decision-1): a textbox carrying BOTH
    /// plain text and an interior tracked change is seen by the SAME opaque node
    /// in both the census and discovery — they share one walk, so the census
    /// reports the hidden revision under the host block and discovery exposes the
    /// editable text under that same host block.
    #[test]
    fn census_and_discovery_agree_on_the_same_opaque() {
        let inner = r#"<w:p><w:r><w:t>Visible text </w:t></w:r><w:ins w:id="1" w:author="Vanessa" w:date="2024-01-01T00:00:00Z"><w:r><w:t>added</w:t></w:r></w:ins></w:p>"#;
        let body = format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        );
        let doc = import_body(&body);

        let census = crate::tracked_model::enumerate_revisions(&doc);
        let opaque_records: Vec<_> = census
            .iter()
            .filter(|r| r.kind == crate::tracked_model::RevisionKind::OpaqueInterior)
            .collect();
        assert!(
            !opaque_records.is_empty(),
            "census must surface the textbox's interior tracked insert"
        );

        let targets = opaque_text_targets(&doc);
        assert!(
            !targets.is_empty(),
            "discovery must surface the textbox's editable text"
        );
        // Same host block in both surfaces — the shared walk visited one opaque.
        assert_eq!(
            opaque_records[0].block_id, targets[0].host_block_id,
            "census and discovery must attribute the same opaque to the same host block"
        );
    }
}
