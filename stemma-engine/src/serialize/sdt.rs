//! Deterministic builder for structured document tag properties (`w:sdtPr`,
//! §17.5.2) from a typed [`SdtControl`]. Owned by the `WrapInContentControl`
//! verb (see `edit/verbs/content_controls.rs`); carved into its own file per
//! `edit/AGENTS.md` so the verb owns these lines.
//!
//! We emit the `w:sdtPr` element as a deterministic XML string (a fixed child
//! order, escaped text) rather than reaching for a generic element builder: the
//! shape is small and fully specified, and a string keeps the output stable for
//! tests and round-trips. The control-kind child names follow ECMA-376 §17.5.2
//! and the MS-DOCX Word extensions (`w14:checkbox`, `w15:repeatingSection`).

use crate::domain::{SdtControl, SdtListItem};
use crate::edit::verbs::content_controls::DataBinding;

/// Namespaces declared on the synthesized `w:sdt` so a stand-alone fragment
/// (parsed via `parse_raw_fragment`) resolves every prefix it uses.
pub(crate) const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
pub(crate) const W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
pub(crate) const W15_NS: &str = "http://schemas.microsoft.com/office/word/2012/wordml";

/// Escape the five XML predefined entities for use in text/attribute content.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Build the inner `w:sdtPr` XML (the `<w:sdtPr>…</w:sdtPr>` element) for the
/// given identity + control, in a fixed child order (ECMA-376 §17.5.2.38
/// `CT_SdtPr`): `[w:alias] [w:tag] [w:id] [w:dataBinding] <control-kind>`.
///
/// `id` is a stable decimal the verb derives from the host node. `tag`/`alias`
/// are optional; the control-kind child is always present (RichText emits no
/// kind child, matching Word's representation of a rich-text control).
///
/// `binding`, when present, emits a `<w:dataBinding>` (§17.5.2.6) after `w:id`
/// and before the control kind — the position Word writes it. The binding's
/// `xpath`/`store_item_id` are pre-validated non-empty at the verb edge
/// (`MalformedDataBinding`), so a `Some` binding here always carries a target.
pub(crate) fn build_sdt_pr(
    id: i32,
    tag: Option<&str>,
    alias: Option<&str>,
    control: &SdtControl,
    binding: Option<&DataBinding>,
) -> String {
    let mut s = String::from("<w:sdtPr>");
    if let Some(alias) = alias {
        s.push_str(&format!(r#"<w:alias w:val="{}"/>"#, esc(alias)));
    }
    if let Some(tag) = tag {
        s.push_str(&format!(r#"<w:tag w:val="{}"/>"#, esc(tag)));
    }
    s.push_str(&format!(r#"<w:id w:val="{id}"/>"#));
    if let Some(b) = binding {
        s.push_str(&build_data_binding(b));
    }
    s.push_str(&build_control_kind(control));
    s.push_str("</w:sdtPr>");
    s
}

/// Build the `<w:dataBinding>` child (§17.5.2.6): the bound XPath, the backing
/// datastore part's `storeItemID`, and an optional `prefixMappings`. Attribute
/// order matches Word's output: `prefixMappings?`, `xpath`, `storeItemID`.
fn build_data_binding(b: &DataBinding) -> String {
    let mut s = String::from("<w:dataBinding ");
    if let Some(pm) = &b.prefix_mappings {
        s.push_str(&format!(r#"w:prefixMappings="{}" "#, esc(pm)));
    }
    s.push_str(&format!(
        r#"w:xpath="{}" w:storeItemID="{}"/>"#,
        esc(&b.xpath),
        esc(&b.store_item_id)
    ));
    s
}

/// Build the control-kind child element for a [`SdtControl`].
fn build_control_kind(control: &SdtControl) -> String {
    match control {
        SdtControl::PlainText => "<w:text/>".to_string(),
        // A rich-text control is the absence of a specific kind child.
        SdtControl::RichText => String::new(),
        SdtControl::Dropdown { items } => {
            format!("<w:dropDownList>{}</w:dropDownList>", list_items(items))
        }
        SdtControl::ComboBox { items } => {
            format!("<w:comboBox>{}</w:comboBox>", list_items(items))
        }
        SdtControl::Checkbox { checked } => {
            // w14 checkbox: <w14:checkbox><w14:checked w14:val="1"/></w14:checkbox>.
            format!(
                r#"<w14:checkbox><w14:checked w14:val="{}"/></w14:checkbox>"#,
                if *checked { 1 } else { 0 }
            )
        }
        SdtControl::Date => "<w:date/>".to_string(),
        SdtControl::RepeatingSection => "<w15:repeatingSection/>".to_string(),
    }
}

/// Render the `w:listItem` children for a drop-down / combo box.
fn list_items(items: &[SdtListItem]) -> String {
    let mut s = String::new();
    for item in items {
        s.push_str(&format!(
            r#"<w:listItem w:displayText="{}" w:value="{}"/>"#,
            esc(&item.display),
            esc(&item.value)
        ));
    }
    s
}

/// Build a complete inline `w:sdt` element wrapping `inner_content_xml` (the
/// already-serialized run(s) to go inside `w:sdtContent`). Namespaces are
/// declared on the `w:sdt` so the fragment resolves stand-alone.
pub(crate) fn build_inline_sdt(
    id: i32,
    tag: Option<&str>,
    alias: Option<&str>,
    control: &SdtControl,
    binding: Option<&DataBinding>,
    inner_content_xml: &str,
) -> String {
    format!(
        r#"<w:sdt xmlns:w="{W_NS}" xmlns:w14="{W14_NS}" xmlns:w15="{W15_NS}">{pr}<w:sdtContent>{inner}</w:sdtContent></w:sdt>"#,
        pr = build_sdt_pr(id, tag, alias, control, binding),
        inner = inner_content_xml,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_carries_tag_alias_and_text_kind() {
        let pr = build_sdt_pr(
            7,
            Some("party"),
            Some("Counterparty"),
            &SdtControl::PlainText,
            None,
        );
        assert!(pr.contains(r#"<w:alias w:val="Counterparty"/>"#));
        assert!(pr.contains(r#"<w:tag w:val="party"/>"#));
        assert!(pr.contains(r#"<w:id w:val="7"/>"#));
        assert!(pr.contains("<w:text/>"));
        // alias precedes tag precedes id (fixed order).
        let a = pr.find("w:alias").unwrap();
        let t = pr.find("w:tag").unwrap();
        let i = pr.find("w:id").unwrap();
        assert!(a < t && t < i);
    }

    #[test]
    fn rich_text_emits_no_kind_child() {
        let pr = build_sdt_pr(1, None, None, &SdtControl::RichText, None);
        assert!(!pr.contains("<w:text"));
        assert!(!pr.contains("<w:dropDownList"));
        assert!(pr.contains(r#"<w:id w:val="1"/>"#));
    }

    #[test]
    fn dropdown_lists_each_item() {
        let pr = build_sdt_pr(
            2,
            None,
            None,
            &SdtControl::Dropdown {
                items: vec![
                    SdtListItem {
                        display: "Yes".into(),
                        value: "Y".into(),
                    },
                    SdtListItem {
                        display: "No".into(),
                        value: "N".into(),
                    },
                ],
            },
            None,
        );
        assert!(pr.contains(r#"<w:listItem w:displayText="Yes" w:value="Y"/>"#));
        assert!(pr.contains(r#"<w:listItem w:displayText="No" w:value="N"/>"#));
    }

    #[test]
    fn checkbox_reflects_checked_state() {
        let on = build_sdt_pr(3, None, None, &SdtControl::Checkbox { checked: true }, None);
        assert!(on.contains(r#"<w14:checked w14:val="1"/>"#));
        let off = build_sdt_pr(
            3,
            None,
            None,
            &SdtControl::Checkbox { checked: false },
            None,
        );
        assert!(off.contains(r#"<w14:checked w14:val="0"/>"#));
    }

    #[test]
    fn special_chars_are_escaped() {
        let pr = build_sdt_pr(4, Some("a&b<c"), Some(r#"q"uote"#), &SdtControl::Date, None);
        assert!(pr.contains("a&amp;b&lt;c"));
        assert!(pr.contains("q&quot;uote"));
        assert!(pr.contains("<w:date/>"));
    }

    #[test]
    fn inline_sdt_declares_namespaces_and_wraps_content() {
        let sdt = build_inline_sdt(
            5,
            Some("t"),
            None,
            &SdtControl::PlainText,
            None,
            r#"<w:r><w:t>hello</w:t></w:r>"#,
        );
        assert!(sdt.starts_with("<w:sdt "));
        assert!(sdt.contains(&format!(r#"xmlns:w="{W_NS}""#)));
        assert!(sdt.contains("<w:sdtContent><w:r><w:t>hello</w:t></w:r></w:sdtContent>"));
    }

    #[test]
    fn data_binding_emitted_after_id_before_control_kind() {
        let binding = DataBinding {
            xpath: "/ns0:root[1]/ns0:party[1]".to_string(),
            store_item_id: "{11111111-2222-3333-4444-555555555555}".to_string(),
            prefix_mappings: Some("xmlns:ns0='urn:contract'".to_string()),
        };
        let pr = build_sdt_pr(
            9,
            Some("party"),
            None,
            &SdtControl::PlainText,
            Some(&binding),
        );
        // dataBinding carries the xpath + storeItemID + prefixMappings.
        assert!(pr.contains(r#"w:xpath="/ns0:root[1]/ns0:party[1]""#));
        assert!(pr.contains(r#"w:storeItemID="{11111111-2222-3333-4444-555555555555}""#));
        assert!(pr.contains(r#"w:prefixMappings="xmlns:ns0=&apos;urn:contract&apos;""#));
        // Order: w:id precedes w:dataBinding precedes the control kind (w:text).
        let id = pr.find("<w:id").unwrap();
        let db = pr.find("<w:dataBinding").unwrap();
        let text = pr.find("<w:text/>").unwrap();
        assert!(
            id < db && db < text,
            "sdtPr order: id < dataBinding < control kind"
        );
    }

    #[test]
    fn no_binding_emits_no_data_binding() {
        let pr = build_sdt_pr(1, Some("t"), None, &SdtControl::PlainText, None);
        assert!(!pr.contains("w:dataBinding"));
    }
}
