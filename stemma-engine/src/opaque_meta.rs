//! Read-only metadata projection for opaque inline anchors.
//!
//! An opaque inline (`OpaqueInlineNode`) carries its human-meaningful detail in
//! one of two places: typed IR data (a `Field`/`Hyperlink`/`Sym`/note reference
//! already has structured fields), or — for `Sdt` and `Drawing` — only in
//! `raw_xml`. This module projects both into a single typed [`OpaqueMetadata`]
//! enum for the read surface, so an agent driving stemma can SEE what an opaque
//! is (a "Tenant Name" plain-text control, a 3.2 cm logo with alt text, a
//! FORMTEXT field named `Party`) instead of an anonymous `<obj id=o_42/>`.
//!
//! ## Read-only by contract
//!
//! This module owns NO mutation API. The write path stays in
//! `edit::verbs::content_controls` / `edit::verbs::images`. The shared layer here
//! is read primitives only — parse the fragment, walk elements by local name,
//! extract owned values — so a read can never mutate a node. The verb layer's
//! control-kind detection is re-expressed against the [`sdt_control_kind`] /
//! [`sdt_list_items`] primitives exported here (one parser, two callers).
//!
//! ## No silent fallbacks (CLAUDE.md prime directive)
//!
//! [`project`] matches every `OpaqueKind` explicitly — there is NO wildcard arm.
//! A kind that carries nothing discoverable returns `None` (documented
//! bareness). A
//! kind that SHOULD carry metadata but whose `raw_xml` is missing or unparseable
//! returns `OpaqueMetadata::Unparsed { reason }` — a visible failure, never a
//! silent empty. Adding a variant to `OpaqueKind` forces a compile error here
//! until the author decides surfaced-vs-bare.

use crate::domain::{FieldKind, OpaqueInlineNode, OpaqueKind};
use crate::view::{
    FieldCharRole, FormFieldIdentity, OpaqueMetadata, SdtControlKind, SdtListItemView,
};
use crate::word_xml::parse_raw_fragment;
use xmltree::{Element, XMLNode};

/// Project an opaque inline's discoverable metadata for the read surface.
///
/// Returns `None` for kinds with nothing useful to surface (documented
/// bareness). Returns `Some(OpaqueMetadata::Unparsed { .. })` when a kind that
/// SHOULD carry metadata has `raw_xml` that is missing or fails to parse — never
/// a silent empty.
///
/// ## Caching decision
///
/// The new read-path cost is one `parse_raw_fragment` per metadata-bearing
/// opaque (Sdt/Drawing). Measured on a synthetic 200-SDT document (release,
/// best-of-5), the denominator being total `build_document_view` latency — the
/// walk `find` / `read_block` perform:
/// - WITHOUT projection: ~1.4 ms
/// - WITH projection:    ~44 ms  (projection ≈ 97% of the walk, ~42 ms added)
///
/// The threshold to add a `content_hash`-keyed cache is *both* projection
/// over 50% of latency AND absolute added cost over 50 ms. The fraction crosses
/// (97%) but the absolute cost (~42 ms for 200 controls, an upper bound — real
/// forms are tens-to-low-hundreds) sits *below* 50 ms, so the conjunctive rule
/// is NOT met → **no cache** (IR stays pure). Re-litigate only if a bulk
/// form-fill caller pushes a real doc's projection over ~50 ms absolute; the
/// cache then belongs in the stemma-mcp runtime layer keyed on `content_hash`,
/// never on the IR node.
pub(crate) fn project(node: &OpaqueInlineNode) -> Option<OpaqueMetadata> {
    match &node.kind {
        OpaqueKind::Sdt => Some(project_sdt(node)),
        OpaqueKind::Drawing => Some(project_drawing(node)),
        OpaqueKind::Field(data) => Some(project_field(node, data)),
        OpaqueKind::Hyperlink(data) => Some(OpaqueMetadata::Hyperlink {
            url: data.url.clone(),
            anchor: data.anchor.clone(),
        }),
        OpaqueKind::CommentReference(d)
        | OpaqueKind::FootnoteReference(d)
        | OpaqueKind::EndnoteReference(d) => Some(OpaqueMetadata::NoteReference {
            reference_id: d.reference_id.clone(),
        }),
        OpaqueKind::Sym(d) => Some(OpaqueMetadata::Symbol {
            display_char: d.display_char.to_string(),
            font: d.font.clone(),
        }),
        // Documented bareness — each an explicit `None`, never a wildcard.
        // OMML carries no linear-text projection (§1.6); SmartArt is a diagram
        // part graph with no single label; Ruby/SmartTag/Ptab/CustomXml/Unknown
        // offer no agent action today; Quarantined is bare BY CONTRACT (the read
        // view shows a placeholder, never the inner revisions).
        OpaqueKind::OmmlBlock
        | OpaqueKind::OmmlInline
        | OpaqueKind::SmartArt
        | OpaqueKind::Ruby
        | OpaqueKind::SmartTag
        | OpaqueKind::Ptab
        | OpaqueKind::CustomXml
        | OpaqueKind::Unknown(_)
        | OpaqueKind::QuarantinedNestedTracking => None,
    }
}

// ─── SDT (content control) ────────────────────────────────────────────────────

/// Project a `w:sdt`'s discoverable metadata from `raw_xml`. An SDT always
/// carries its bytes (import sets `raw_xml: Some(..)`, `WrapInContentControl`
/// sets `raw_xml: Some(..)`), so a missing/unparseable `raw_xml` here means a
/// corrupt IR → `Unparsed`, NOT a legitimate empty.
fn project_sdt(node: &OpaqueInlineNode) -> OpaqueMetadata {
    let element = match parse_required_raw(node, "content control") {
        Ok(el) => el,
        Err(reason) => return OpaqueMetadata::Unparsed { reason },
    };

    let pr = child_by_local(&element, "sdtPr");
    let tag = pr.and_then(|p| child_attr_val(p, "tag"));
    let alias = pr.and_then(|p| child_attr_val(p, "alias"));
    let control = sdt_control_kind(&element);
    let list_items = match control {
        SdtControlKind::Dropdown | SdtControlKind::ComboBox => sdt_list_items(&element),
        _ => Vec::new(),
    };
    let checked = match control {
        SdtControlKind::Checkbox => Some(sdt_checkbox_checked(&element)),
        _ => None,
    };
    let display_text = sdt_content_text(&element);

    OpaqueMetadata::ContentControl {
        tag,
        alias,
        control,
        display_text,
        list_items,
        checked,
    }
}

/// Classify a `w:sdt` element's control kind from its `w:sdtPr`. RichText is the
/// absence of a recognized kind child. **Shared read primitive**: the write-path
/// verb (`content_controls::detect_control_kind`) is re-expressed against this.
pub(crate) fn sdt_control_kind(sdt: &Element) -> SdtControlKind {
    let Some(pr) = child_by_local(sdt, "sdtPr") else {
        return SdtControlKind::RichText;
    };
    for child in &pr.children {
        if let XMLNode::Element(el) = child {
            match el.name.as_str() {
                "text" => return SdtControlKind::PlainText,
                "dropDownList" => return SdtControlKind::Dropdown,
                "comboBox" => return SdtControlKind::ComboBox,
                "checkbox" => return SdtControlKind::Checkbox,
                "date" => return SdtControlKind::Date,
                "repeatingSection" => return SdtControlKind::RepeatingSection,
                _ => {}
            }
        }
    }
    SdtControlKind::RichText
}

/// Read the full list-item set of a dropdown/combo `w:sdt`, in document order.
/// **Shared read primitive** for both the read projection and the verb layer's
/// selected-value resolution.
pub(crate) fn sdt_list_items(sdt: &Element) -> Vec<SdtListItemView> {
    let Some(pr) = child_by_local(sdt, "sdtPr") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for kind in ["dropDownList", "comboBox"] {
        if let Some(list) = child_by_local(pr, kind) {
            for child in &list.children {
                if let XMLNode::Element(item) = child
                    && item.name == "listItem"
                {
                    let value = crate::xml_attrs::attr_get(item, "w:value")
                        .cloned()
                        .unwrap_or_default();
                    // Word treats a list item with no displayText as displaying
                    // its value verbatim (the wrap serializer always emits both;
                    // imported documents may omit displayText).
                    let display = crate::xml_attrs::attr_get(item, "w:displayText")
                        .cloned()
                        .unwrap_or_else(|| value.clone());
                    out.push(SdtListItemView { display, value });
                }
            }
        }
    }
    out
}

/// Read the legacy/`w14` checkbox state of a checkbox `w:sdt`
/// (`w14:checkbox` > `w14:checked` @`w14:val`). Absent / `0` → `false`.
fn sdt_checkbox_checked(sdt: &Element) -> bool {
    child_by_local(sdt, "sdtPr")
        .and_then(|pr| child_by_local(pr, "checkbox"))
        .and_then(|cb| child_by_local(cb, "checked"))
        .and_then(|c| crate::xml_attrs::attr_get(c, "w14:val").cloned())
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Concatenate the visible run text inside the control's `w:sdtContent`. `None`
/// when the content is empty (no `w:t` descendants), so the read surface can
/// distinguish "empty control" from "control with a value".
fn sdt_content_text(sdt: &Element) -> Option<String> {
    let content = child_by_local(sdt, "sdtContent")?;
    let mut out = String::new();
    collect_text_descendants(content, &mut out);
    if out.is_empty() { None } else { Some(out) }
}

// ─── Drawing ──────────────────────────────────────────────────────────────────

/// Project an inline `w:drawing`'s extent / alt-text / media rId from `raw_xml`.
fn project_drawing(node: &OpaqueInlineNode) -> OpaqueMetadata {
    let element = match parse_required_raw(node, "drawing") {
        Ok(el) => el,
        Err(reason) => return OpaqueMetadata::Unparsed { reason },
    };

    let extent = descendant_by_local(&element, "extent");
    let extent_cx_emu = extent.and_then(|e| attr_i64(e, "cx"));
    let extent_cy_emu = extent.and_then(|e| attr_i64(e, "cy"));
    // wp:docPr @descr is the accessibility alt text (§20.4.2.5).
    let alt_text = descendant_by_local(&element, "docPr")
        .and_then(|d| crate::xml_attrs::attr_get(d, "descr").cloned())
        .filter(|s| !s.is_empty());
    // a:blip @r:embed is the relationship id of the embedded media part.
    let embed_rid = descendant_by_local(&element, "blip")
        .and_then(|b| crate::xml_attrs::attr_get(b, "r:embed").cloned());
    let textbox_text = textbox_interior_text(&element);

    OpaqueMetadata::Drawing {
        extent_cx_emu,
        extent_cy_emu,
        alt_text,
        embed_rid,
        textbox_text,
    }
}

/// Concatenate the interior text of the textbox(es) carried by a drawing. Each
/// `w:txbxContent` (the common story element of BOTH a DrawingML `wps:txbx` and
/// a VML `v:textbox`) is reduced to its child `w:p` text joined by `\n`; the
/// per-textbox interiors are then joined by `\n`. `None` when the drawing
/// carries no textbox.
///
/// **Multiple/duplicate textboxes** (a DECISION, per review): Word's standard
/// textbox emission wraps the shape in `mc:AlternateContent` with a DrawingML
/// `Choice` AND a VML `Fallback`, each carrying a DUPLICATE `w:txbxContent` —
/// and a group shape can carry several DISTINCT textboxes. So we collect ALL
/// `txbxContent` descendants, DEDUPE byte-identical interiors (the
/// Choice/Fallback duplicate projects ONCE, not doubled), and `\n`-join the
/// remaining DISTINCT interiors in document order (reading every distinct
/// textbox is the honest projection — this is a read, not a mutation, so there
/// is no refusal).
///
/// **Visibility** (v1 read semantics, a DECISION not an accident): we surface
/// the **markup-resolved-as-shown** reading — the text a reviewer sees in Word's
/// markup view. Inserted text (`w:t` inside `w:ins`) AND deleted text
/// (`w:delText` inside `w:del`) are BOTH included, because both are visible in
/// the markup view. `collect_text_descendants` already gathers every text node
/// (`w:t` and `w:delText` alike), so a paragraph's text is its full as-shown
/// content. (When the M3 verb adds resolution, an accept/reject-aware reading
/// can refine this; v1 is the reviewer's-eye reading.)
fn textbox_interior_text(drawing: &Element) -> Option<String> {
    let mut contents = Vec::new();
    collect_descendants_by_local(drawing, "txbxContent", &mut contents);
    if contents.is_empty() {
        return None;
    }
    let mut interiors: Vec<String> = Vec::new();
    for content in contents {
        let mut paragraphs = Vec::new();
        for child in &content.children {
            if let XMLNode::Element(el) = child
                && el.name == "p"
            {
                let mut text = String::new();
                collect_text_descendants(el, &mut text);
                paragraphs.push(text);
            }
        }
        let interior = paragraphs.join("\n");
        // Dedupe identical copies (AlternateContent Choice/Fallback duplicate).
        // Order-preserving: keep the first occurrence, drop later identicals.
        if !interiors.contains(&interior) {
            interiors.push(interior);
        }
    }
    // A textbox with paragraphs but no text yields an empty-string interior,
    // which is still "the textbox exists and is empty" — distinct from no
    // textbox (None, handled above).
    Some(interiors.join("\n"))
}

// ─── Field ────────────────────────────────────────────────────────────────────

/// Project a field anchor: typed IR data plus — only on a Begin anchor carrying
/// a `w:ffData` child — the legacy form-field identity parsed from `raw_xml`.
fn project_field(node: &OpaqueInlineNode, data: &crate::domain::FieldData) -> OpaqueMetadata {
    let field_char = FieldCharRole::from(&data.field_kind);
    let semantic = data.semantic.as_ref().map(field_semantic_label);

    // The form-field identity lives in the BEGIN anchor's w:ffData. A complex
    // legacy form field imports as a flat sequence of separate anchors; only the
    // begin anchor carries ffData. For every other anchor role, `form` is None.
    let form = if matches!(data.field_kind, FieldKind::Begin) {
        match node.raw_xml.as_deref() {
            // No raw_xml on a begin anchor is unusual but not a corruption the
            // way a missing SDT body is — a synthetic begin may lack bytes. We
            // treat "no bytes" as "no ffData to read" (form: None), which is
            // honest: there is no form identity to surface.
            None => None,
            Some(raw) => match parse_raw_fragment(raw) {
                Ok(element) => parse_ff_data(&element),
                // A begin anchor whose bytes are present but unparseable is the
                // §4 failure: surface Unparsed for the whole anchor, never a
                // silent "ordinary field".
                Err(e) => {
                    return OpaqueMetadata::Unparsed {
                        reason: format!("field begin anchor raw_xml parse failed: {e}"),
                    };
                }
            },
        }
    } else {
        None
    };

    OpaqueMetadata::Field {
        field_char,
        instruction: data.instruction_text.clone(),
        result: data.result_text.clone(),
        semantic,
        form,
    }
}

/// Parse a `w:ffData` blob into a [`FormFieldIdentity`]. Returns `None` when the
/// element carries no `ffData` child (an ordinary field: TOC/REF/PAGE begin), or
/// when the `ffData` has no recognized form-field child.
fn parse_ff_data(element: &Element) -> Option<FormFieldIdentity> {
    // ffData may be nested under a w:fldChar wrapper or be the element itself.
    let ff = descendant_by_local(element, "ffData")?;
    let name =
        child_by_local(ff, "name").and_then(|n| crate::xml_attrs::attr_get(n, "w:val").cloned());

    if let Some(text_input) = child_by_local(ff, "textInput") {
        let default = child_by_local(text_input, "default")
            .and_then(|d| crate::xml_attrs::attr_get(d, "w:val").cloned());
        return Some(FormFieldIdentity::TextInput { name, default });
    }
    if let Some(check_box) = child_by_local(ff, "checkBox") {
        // Legacy checkbox: the live state is <w:checked w:val>, defaulting to
        // <w:default w:val> when no explicit state is stored.
        let checked = child_by_local(check_box, "checked")
            .and_then(|c| bool_attr(c, "w:val"))
            .or_else(|| child_by_local(check_box, "default").and_then(|d| bool_attr(d, "w:val")))
            .unwrap_or(false);
        return Some(FormFieldIdentity::Checkbox { name, checked });
    }
    if let Some(dd_list) = child_by_local(ff, "ddList") {
        let mut entries = Vec::new();
        for child in &dd_list.children {
            if let XMLNode::Element(el) = child
                && el.name == "listEntry"
                && let Some(v) = crate::xml_attrs::attr_get(el, "w:val")
            {
                entries.push(v.clone());
            }
        }
        let selected_index = child_by_local(dd_list, "result")
            .and_then(|r| crate::xml_attrs::attr_get(r, "w:val").cloned())
            .and_then(|v| v.parse::<usize>().ok());
        return Some(FormFieldIdentity::DropDown {
            name,
            entries,
            selected_index,
        });
    }
    None
}

// ─── shared helpers ───────────────────────────────────────────────────────────

/// Parse the `raw_xml` of a kind that REQUIRES it (Sdt/Drawing). A missing or
/// unparseable `raw_xml` is the §4 failure → the caller surfaces `Unparsed`,
/// never a silent empty. Returns the failure reason on `Err` (the caller wraps
/// it in `OpaqueMetadata::Unparsed`).
fn parse_required_raw(node: &OpaqueInlineNode, what: &str) -> Result<Element, String> {
    let raw = node
        .raw_xml
        .as_deref()
        .ok_or_else(|| format!("{what} has no raw_xml"))?;
    parse_raw_fragment(raw).map_err(|e| format!("{what} raw_xml parse failed: {e}"))
}

/// First direct child element with the given local name (xmltree-ns: `name` is
/// the local name).
fn child_by_local<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|c| match c {
        XMLNode::Element(el) if el.name == local => Some(el),
        _ => None,
    })
}

/// `@w:val` of the first direct child element with the given local name (the
/// `<w:tag w:val="..">` / `<w:alias w:val="..">` shape).
fn child_attr_val(parent: &Element, local: &str) -> Option<String> {
    child_by_local(parent, local).and_then(|el| crate::xml_attrs::attr_get(el, "w:val").cloned())
}

/// Depth-first search for the first descendant element (or `root` itself) whose
/// local name equals `local`. Read-only twin of `images::find_descendant_by_local_mut`.
fn descendant_by_local<'a>(root: &'a Element, local: &str) -> Option<&'a Element> {
    if root.name == local {
        return Some(root);
    }
    for child in &root.children {
        if let XMLNode::Element(el) = child
            && let Some(hit) = descendant_by_local(el, local)
        {
            return Some(hit);
        }
    }
    None
}

/// Collect every descendant element (and `root` itself) whose local name equals
/// `local`, in document order. Unlike [`descendant_by_local`] this gathers ALL
/// matches (a drawing may carry several textboxes, or duplicate Choice/Fallback
/// copies). Once a match is found we do NOT recurse INTO it — a `txbxContent`'s
/// own paragraphs may host a *nested* drawing with its own textbox, which
/// belongs to that nested anchor's story, not this one.
///
/// **Shared read primitive** (one helper, two callers — `CLAUDE.md` "one good
/// way"): this read projection and the `set_textbox_text` verb
/// (`edit::verbs::textbox`) both locate the `w:txbxContent` copies a textbox
/// carries; the verb calls this rather than duplicating the walk, mirroring how
/// `content_controls` re-expresses against [`sdt_control_kind`]. The no-recurse
/// rule also keeps the verb from miscounting a *nested* anchor's textbox as a
/// copy of its own.
pub(crate) fn collect_descendants_by_local<'a>(
    root: &'a Element,
    local: &str,
    out: &mut Vec<&'a Element>,
) {
    if root.name == local {
        out.push(root);
        return;
    }
    for child in &root.children {
        if let XMLNode::Element(el) = child {
            collect_descendants_by_local(el, local, out);
        }
    }
}

/// Concatenate every `XMLNode::Text` descendant of `element`, in document order.
fn collect_text_descendants(element: &Element, out: &mut String) {
    for child in &element.children {
        match child {
            XMLNode::Text(t) => out.push_str(t),
            XMLNode::Element(el) => collect_text_descendants(el, out),
            _ => {}
        }
    }
}

fn attr_i64(element: &Element, qname: &str) -> Option<i64> {
    crate::xml_attrs::attr_get(element, qname).and_then(|v| v.parse::<i64>().ok())
}

/// Parse an OOXML boolean attribute (`1`/`0`/`true`/`false`/`on`/`off`).
fn bool_attr(element: &Element, qname: &str) -> Option<bool> {
    crate::xml_attrs::attr_get(element, qname)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on"))
}

/// Render a `FieldSemantic` to a short label for the read surface. The full
/// structured spec stays in the IR; the read surface wants a one-word hint.
fn field_semantic_label(semantic: &crate::domain::FieldSemantic) -> String {
    use crate::domain::FieldSemantic;
    match semantic {
        FieldSemantic::Toc(_) => "toc".to_string(),
        FieldSemantic::Hyperlink(_) => "hyperlink".to_string(),
        FieldSemantic::MergeField(_) => "merge_field".to_string(),
        FieldSemantic::Ref(_) => "ref".to_string(),
        FieldSemantic::DateTime(_) => "date_time".to_string(),
        FieldSemantic::If(_) => "if".to_string(),
        FieldSemantic::Formula(_) => "formula".to_string(),
        FieldSemantic::Other { field_name, .. } => field_name.clone(),
    }
}

impl From<&FieldKind> for FieldCharRole {
    fn from(kind: &FieldKind) -> Self {
        match kind {
            FieldKind::Begin => FieldCharRole::Begin,
            FieldKind::Instruction => FieldCharRole::Instruction,
            FieldKind::Separate => FieldCharRole::Separate,
            FieldKind::End => FieldCharRole::End,
            FieldKind::Simple => FieldCharRole::Simple,
            // The raw type string is preserved in raw_xml and is not
            // action-relevant for read-surfacing.
            FieldKind::Unknown(_) => FieldCharRole::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        DocPart, FieldData, FieldKind, HyperlinkData, NodeId, NoteReferenceData, ProofRef,
        SdtControl, SdtListItem, StyleProps, SymData,
    };
    use crate::serialize::sdt::build_inline_sdt;

    /// Build an `OpaqueInlineNode` carrying the given kind + raw bytes — the
    /// shape the read projection consumes.
    fn node(kind: OpaqueKind, raw_xml: Option<Vec<u8>>) -> OpaqueInlineNode {
        OpaqueInlineNode {
            id: NodeId::from("o_test"),
            kind,
            opaque_ref: "ref".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("o_test"),
                docx_anchor: String::new(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml,
            content_hash: None,
        }
    }

    /// A `w:sdt` with the given control, tag/alias and inner content text.
    fn sdt_node(
        tag: Option<&str>,
        alias: Option<&str>,
        control: &SdtControl,
        inner_text: &str,
    ) -> OpaqueInlineNode {
        let inner = format!(r#"<w:r><w:t xml:space="preserve">{inner_text}</w:t></w:r>"#);
        let xml = build_inline_sdt(1, tag, alias, control, None, &inner);
        node(OpaqueKind::Sdt, Some(xml.into_bytes()))
    }

    #[test]
    fn project_sdt_plain_text_surfaces_tag_alias_value() {
        let n = sdt_node(
            Some("TenantName"),
            Some("Tenant Name"),
            &SdtControl::PlainText,
            "Acme Corporation",
        );
        match project(&n).expect("sdt surfaces metadata") {
            OpaqueMetadata::ContentControl {
                tag,
                alias,
                control,
                display_text,
                list_items,
                checked,
            } => {
                assert_eq!(tag.as_deref(), Some("TenantName"));
                assert_eq!(alias.as_deref(), Some("Tenant Name"));
                assert_eq!(control, SdtControlKind::PlainText);
                assert_eq!(display_text.as_deref(), Some("Acme Corporation"));
                assert!(list_items.is_empty());
                assert_eq!(checked, None);
            }
            other => panic!("expected ContentControl, got {other:?}"),
        }
    }

    #[test]
    fn project_sdt_dropdown_surfaces_list_items() {
        let n = sdt_node(
            Some("Country"),
            None,
            &SdtControl::Dropdown {
                items: vec![
                    SdtListItem {
                        display: "United States".into(),
                        value: "US".into(),
                    },
                    SdtListItem {
                        display: "Norway".into(),
                        value: "NO".into(),
                    },
                ],
            },
            "United States",
        );
        match project(&n).expect("dropdown surfaces metadata") {
            OpaqueMetadata::ContentControl {
                control,
                list_items,
                ..
            } => {
                assert_eq!(control, SdtControlKind::Dropdown);
                assert_eq!(list_items.len(), 2);
                assert_eq!(list_items[0].display, "United States");
                assert_eq!(list_items[0].value, "US");
                assert_eq!(list_items[1].display, "Norway");
                assert_eq!(list_items[1].value, "NO");
            }
            other => panic!("expected ContentControl, got {other:?}"),
        }
    }

    #[test]
    fn project_sdt_checkbox_surfaces_checked() {
        let n = sdt_node(
            None,
            None,
            &SdtControl::Checkbox { checked: true },
            "\u{2612}",
        );
        match project(&n).expect("checkbox surfaces metadata") {
            OpaqueMetadata::ContentControl {
                control, checked, ..
            } => {
                assert_eq!(control, SdtControlKind::Checkbox);
                assert_eq!(checked, Some(true));
            }
            other => panic!("expected ContentControl, got {other:?}"),
        }

        let unchecked = sdt_node(
            None,
            None,
            &SdtControl::Checkbox { checked: false },
            "\u{2610}",
        );
        match project(&unchecked).expect("checkbox surfaces metadata") {
            OpaqueMetadata::ContentControl { checked, .. } => assert_eq!(checked, Some(false)),
            other => panic!("expected ContentControl, got {other:?}"),
        }
    }

    #[test]
    fn project_drawing_surfaces_extent_alt_embed() {
        // Mirror the images.rs drawing fragment shape, with a blip embed rId.
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:inline><wp:extent cx="1143000" cy="685800"/><wp:docPr id="1" name="Picture 1" descr="Acme logo"/><a:graphic><a:graphicData><a:blip r:embed="rId5"/></a:graphicData></a:graphic></wp:inline></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        match project(&n).expect("drawing surfaces metadata") {
            OpaqueMetadata::Drawing {
                extent_cx_emu,
                extent_cy_emu,
                alt_text,
                embed_rid,
                textbox_text,
            } => {
                assert_eq!(extent_cx_emu, Some(1_143_000));
                assert_eq!(extent_cy_emu, Some(685_800));
                assert_eq!(alt_text.as_deref(), Some("Acme logo"));
                assert_eq!(embed_rid.as_deref(), Some("rId5"));
                // A picture drawing has no textbox.
                assert_eq!(textbox_text, None);
            }
            other => panic!("expected Drawing, got {other:?}"),
        }
    }

    /// The `textbox_text` of a `Drawing`, or panic if not a Drawing.
    fn drawing_textbox_text(n: &OpaqueInlineNode) -> Option<String> {
        match project(n).expect("drawing surfaces metadata") {
            OpaqueMetadata::Drawing { textbox_text, .. } => textbox_text,
            other => panic!("expected Drawing, got {other:?}"),
        }
    }

    #[test]
    fn project_drawing_surfaces_drawingml_textbox_text() {
        // DrawingML textbox: w:drawing > ... > wps:txbx > w:txbxContent > w:p.
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Hello textbox</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></wp:inline></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(drawing_textbox_text(&n).as_deref(), Some("Hello textbox"));
    }

    #[test]
    fn project_drawing_surfaces_vml_textbox_text() {
        // VML textbox: w:pict > v:shape > v:textbox > w:txbxContent > w:p. The
        // common element across carriers is w:txbxContent — located by local name,
        // so VML and DrawingML both surface. (The opaque kind for a w:pict is
        // Drawing in the IR; here we feed the w:txbxContent under a v:textbox.)
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:v="urn:schemas-microsoft-com:vml"><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>VML text here</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(drawing_textbox_text(&n).as_deref(), Some("VML text here"));
    }

    #[test]
    fn project_drawing_no_textbox_is_none() {
        // A plain picture drawing (no txbxContent) → textbox_text None.
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><wp:inline><wp:extent cx="1" cy="1"/><a:graphic/></wp:inline></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(drawing_textbox_text(&n), None);
    }

    #[test]
    fn project_drawing_multi_paragraph_textbox_joined_with_newline() {
        // Multiple w:p inside the textbox join with '\n'.
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:txbx><w:txbxContent><w:p><w:r><w:t>Line one</w:t></w:r></w:p><w:p><w:r><w:t>Line </w:t></w:r><w:r><w:t>two</w:t></w:r></w:p></w:txbxContent></wps:txbx></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(
            drawing_textbox_text(&n).as_deref(),
            Some("Line one\nLine two")
        );
    }

    #[test]
    fn project_drawing_textbox_includes_inserted_and_deleted_text() {
        // The v1 as-shown reading: text in w:ins AND w:del/w:delText are BOTH
        // surfaced (the markup-view reading). One paragraph: "keep" + inserted
        // " new" + deleted " old".
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:txbx><w:txbxContent><w:p><w:r><w:t>keep</w:t></w:r><w:ins><w:r><w:t> new</w:t></w:r></w:ins><w:del><w:r><w:delText> old</w:delText></w:r></w:del></w:p></w:txbxContent></wps:txbx></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(drawing_textbox_text(&n).as_deref(), Some("keep new old"));
    }

    #[test]
    fn project_drawing_empty_textbox_is_some_empty_not_none() {
        // A textbox that exists but holds an empty paragraph → Some("") (the
        // textbox is present), distinct from no textbox (None).
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:txbx><w:txbxContent><w:p/></w:txbxContent></wps:txbx></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(drawing_textbox_text(&n).as_deref(), Some(""));
    }

    #[test]
    fn project_drawing_alternatecontent_duplicate_textbox_appears_once() {
        // Word's standard textbox: mc:AlternateContent with a DrawingML Choice
        // AND a VML Fallback, each carrying an IDENTICAL w:txbxContent. The
        // duplicate must project ONCE (deduped), not doubled.
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml"><mc:AlternateContent><mc:Choice Requires="wps"><wps:txbx><w:txbxContent><w:p><w:r><w:t>Shared label</w:t></w:r></w:p></w:txbxContent></wps:txbx></mc:Choice><mc:Fallback><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>Shared label</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></mc:Fallback></mc:AlternateContent></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(drawing_textbox_text(&n).as_deref(), Some("Shared label"));
    }

    #[test]
    fn project_drawing_two_distinct_textboxes_joined_with_newline() {
        // A group shape carrying two DISTINCT textboxes: read both, \n-joined in
        // document order (honest projection — this is a read, no refusal).
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wpg:wgp><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>First box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Second box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></wpg:wgp></w:drawing>"#.to_vec();
        let n = node(OpaqueKind::Drawing, Some(raw));
        assert_eq!(
            drawing_textbox_text(&n).as_deref(),
            Some("First box\nSecond box")
        );
    }

    #[test]
    fn project_field_uses_typed_data_no_parse() {
        // A Simple field with no raw_xml still projects from typed IR data.
        let n = node(
            OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Simple,
                instruction_text: Some(" PAGE ".into()),
                result_text: Some("3".into()),
                semantic: None,
            }),
            None,
        );
        match project(&n).expect("field surfaces metadata") {
            OpaqueMetadata::Field {
                field_char,
                instruction,
                result,
                form,
                ..
            } => {
                assert_eq!(field_char, FieldCharRole::Simple);
                assert_eq!(instruction.as_deref(), Some(" PAGE "));
                assert_eq!(result.as_deref(), Some("3"));
                // Not a Begin anchor → never a form identity.
                assert_eq!(form, None);
            }
            other => panic!("expected Field, got {other:?}"),
        }
    }

    #[test]
    fn project_field_begin_with_ffdata_surfaces_form_identity() {
        // A FORMTEXT begin anchor: w:fldChar with a w:ffData/textInput child.
        let raw = br#"<w:fldChar xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:fldCharType="begin"><w:ffData><w:name w:val="PartyName"/><w:textInput><w:default w:val="ACME"/></w:textInput></w:ffData></w:fldChar>"#.to_vec();
        let n = node(
            OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Begin,
                instruction_text: None,
                result_text: None,
                semantic: None,
            }),
            Some(raw),
        );
        match project(&n).expect("field begin surfaces metadata") {
            OpaqueMetadata::Field {
                field_char,
                form: Some(FormFieldIdentity::TextInput { name, default }),
                ..
            } => {
                assert_eq!(field_char, FieldCharRole::Begin);
                assert_eq!(name.as_deref(), Some("PartyName"));
                assert_eq!(default.as_deref(), Some("ACME"));
            }
            other => panic!("expected Field with TextInput form, got {other:?}"),
        }
    }

    #[test]
    fn project_field_begin_dropdown_ffdata_surfaces_entries() {
        let raw = br#"<w:fldChar xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:fldCharType="begin"><w:ffData><w:name w:val="Choice"/><w:ddList><w:result w:val="1"/><w:listEntry w:val="Alpha"/><w:listEntry w:val="Beta"/></w:ddList></w:ffData></w:fldChar>"#.to_vec();
        let n = node(
            OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Begin,
                instruction_text: None,
                result_text: None,
                semantic: None,
            }),
            Some(raw),
        );
        match project(&n).expect("field begin surfaces metadata") {
            OpaqueMetadata::Field {
                form:
                    Some(FormFieldIdentity::DropDown {
                        name,
                        entries,
                        selected_index,
                    }),
                ..
            } => {
                assert_eq!(name.as_deref(), Some("Choice"));
                assert_eq!(entries, vec!["Alpha".to_string(), "Beta".to_string()]);
                assert_eq!(selected_index, Some(1));
            }
            other => panic!("expected Field with DropDown form, got {other:?}"),
        }
    }

    #[test]
    fn project_field_begin_without_ffdata_is_ordinary_field() {
        // An ordinary field's begin (TOC/REF/PAGE) carries no ffData → form: None,
        // legitimately (NOT Unparsed — the bytes parsed fine).
        let raw = br#"<w:fldChar xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:fldCharType="begin"/>"#.to_vec();
        let n = node(
            OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Begin,
                instruction_text: None,
                result_text: None,
                semantic: None,
            }),
            Some(raw),
        );
        match project(&n).expect("field begin surfaces metadata") {
            OpaqueMetadata::Field {
                field_char, form, ..
            } => {
                assert_eq!(field_char, FieldCharRole::Begin);
                assert_eq!(form, None);
            }
            other => panic!("expected Field, got {other:?}"),
        }
    }

    #[test]
    fn project_field_begin_malformed_ffdata_is_unparsed() {
        // A begin anchor whose bytes are present but unparseable → Unparsed,
        // never a silent "ordinary field" (§A.4).
        let n = node(
            OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Begin,
                instruction_text: None,
                result_text: None,
                semantic: None,
            }),
            Some(b"<w:fldChar".to_vec()),
        );
        match project(&n).expect("field begin surfaces metadata") {
            OpaqueMetadata::Unparsed { reason } => assert!(!reason.is_empty()),
            other => panic!("expected Unparsed, got {other:?}"),
        }
    }

    #[test]
    fn project_hyperlink_surfaces_url_and_anchor() {
        let n = node(
            OpaqueKind::Hyperlink(HyperlinkData {
                url: Some("https://example.com".into()),
                anchor: Some("section2".into()),
                text: "click".into(),
                r_id: Some("rId7".into()),
                runs: Vec::new(),
                extra_attrs: Vec::new(),
            }),
            None,
        );
        match project(&n).expect("hyperlink surfaces metadata") {
            OpaqueMetadata::Hyperlink { url, anchor } => {
                assert_eq!(url.as_deref(), Some("https://example.com"));
                assert_eq!(anchor.as_deref(), Some("section2"));
            }
            other => panic!("expected Hyperlink, got {other:?}"),
        }
    }

    #[test]
    fn project_sym_surfaces_char_and_font() {
        let n = node(
            OpaqueKind::Sym(SymData {
                font: "Wingdings".into(),
                char_code: "F0FC".into(),
                display_char: '\u{2714}',
            }),
            None,
        );
        match project(&n).expect("sym surfaces metadata") {
            OpaqueMetadata::Symbol { display_char, font } => {
                assert_eq!(display_char, "\u{2714}");
                assert_eq!(font, "Wingdings");
            }
            other => panic!("expected Symbol, got {other:?}"),
        }
    }

    #[test]
    fn project_note_reference_surfaces_reference_id() {
        let n = node(
            OpaqueKind::FootnoteReference(NoteReferenceData {
                reference_id: "5".into(),
            }),
            None,
        );
        match project(&n).expect("note reference surfaces metadata") {
            OpaqueMetadata::NoteReference { reference_id } => assert_eq!(reference_id, "5"),
            other => panic!("expected NoteReference, got {other:?}"),
        }
    }

    #[test]
    fn project_bare_kinds_return_none() {
        // Documented bareness: each is an explicit None arm, NOT a wildcard.
        for kind in [
            OpaqueKind::SmartArt,
            OpaqueKind::Ruby,
            OpaqueKind::SmartTag,
            OpaqueKind::Ptab,
            OpaqueKind::CustomXml,
            OpaqueKind::Unknown("w:foo".into()),
            OpaqueKind::QuarantinedNestedTracking,
            OpaqueKind::OmmlBlock,
            OpaqueKind::OmmlInline,
        ] {
            // Even with raw_xml present, bare kinds project to None.
            let n = node(kind.clone(), Some(b"<w:foo/>".to_vec()));
            assert_eq!(project(&n), None, "{kind:?} must be bare (None)");
        }
    }

    #[test]
    fn project_sdt_missing_raw_xml_is_unparsed_not_none() {
        // A corrupt SDT (no raw_xml) must surface Unparsed — NOT None, which
        // would hide a corruption as documented bareness.
        let n = node(OpaqueKind::Sdt, None);
        match project(&n).expect("sdt always projects Some") {
            OpaqueMetadata::Unparsed { reason } => assert!(reason.contains("no raw_xml")),
            other => panic!("expected Unparsed, got {other:?}"),
        }
    }

    #[test]
    fn project_sdt_malformed_raw_xml_is_unparsed() {
        // Malformed bytes → Unparsed with a non-empty reason, NOT a panic, NOT None.
        let n = node(OpaqueKind::Sdt, Some(b"<w:sdt".to_vec()));
        match project(&n).expect("sdt always projects Some") {
            OpaqueMetadata::Unparsed { reason } => assert!(!reason.is_empty()),
            other => panic!("expected Unparsed, got {other:?}"),
        }
    }

    #[test]
    fn project_sdt_empty_content_has_none_display_text() {
        // An empty control distinguishes itself from one with a value.
        let n = sdt_node(Some("Empty"), None, &SdtControl::PlainText, "");
        match project(&n).expect("sdt surfaces metadata") {
            OpaqueMetadata::ContentControl { display_text, .. } => {
                assert_eq!(display_text, None)
            }
            other => panic!("expected ContentControl, got {other:?}"),
        }
    }

    /// Microbench: the new read-path cost is one `parse_raw_fragment` per
    /// metadata-bearing opaque. A form is tens-to-low-hundreds of controls. This
    /// asserts a generous, non-flaky ceiling and prints the measured cost so a
    /// regression is visible. NOTE: projection IS the dominant cost of a
    /// `find` / `read_block` walk on an SDT-heavy doc (~97% of
    /// `build_document_view` on a 200-SDT doc, ~42 ms added) — but that absolute
    /// cost is below the 50 ms cache threshold for a 200-control upper
    /// bound, so no cache is added. Full denominator measurement + decision live
    /// in the `project` doc comment above.
    #[test]
    fn microbench_project_200_sdts_is_cheap() {
        let nodes: Vec<OpaqueInlineNode> = (0..200)
            .map(|i| {
                sdt_node(
                    Some(&format!("Field{i}")),
                    Some(&format!("Field {i}")),
                    &SdtControl::PlainText,
                    &format!("Value {i}"),
                )
            })
            .collect();

        let start = std::time::Instant::now();
        let mut surfaced = 0usize;
        for n in &nodes {
            if let Some(OpaqueMetadata::ContentControl { .. }) = project(n) {
                surfaced += 1;
            }
        }
        let elapsed = start.elapsed();
        assert_eq!(surfaced, 200, "every SDT projects metadata");

        // Print for visibility (`cargo test -- --nocapture`).
        println!(
            "microbench: project() over 200 SDTs took {:?} ({:?} per opaque)",
            elapsed,
            elapsed / 200
        );
        // Generous ceiling: even at ~50µs/parse this is 10ms; CI machines vary,
        // so 250ms is a non-flaky guard that still catches an order-of-magnitude
        // regression (e.g. accidental O(n^2) or a re-parse-per-field blowup).
        assert!(
            elapsed.as_millis() < 250,
            "200-SDT projection unexpectedly slow: {elapsed:?}"
        );
    }
}
