//! `CreateStyle` / `ModifyStyle` — package-level style-table authoring (§17.7.4).
//!
//! These verbs author a `w:style` definition into `word/styles.xml`. They do
//! **not** mutate the body IR: a style definition is a package part, not a
//! paragraph property. The pure verb core has no `DocxPackage` in scope, so it
//! stages a [`crate::edit::StyleOp`] into `PendingParts`; the save path
//! (`runtime::apply_pending_style_ops`) splices the fragment into the styles part
//! AFTER the base/target style merge, so an authored style wins a style-id
//! collision.
//!
//! ## Untracked
//!
//! OOXML has no tracked-change envelope for a style-table edit (there is no
//! `w:styleChange`). Like the metadata verbs, reversibility is at the
//! transaction-rejection level (don't apply the transaction), not at
//! segment-accept/reject. They are [`crate::edit::EditStep`]s so they stay
//! replayable as part of the transaction.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - empty `style_id`                 → `StyleDefEmptyId`
//! - empty `name`                     → `StyleDefEmptyName`
//! - ModifyStyle `style_id` disagrees with `def.style_id` → `StyleDefIdMismatch`
//! - Create of an existing styleId / Modify of an absent one are caught at the
//!   save path (the runtime fails loud there); the verb's job is to stage a
//!   well-formed op.

use super::super::EditError;
use crate::edit::StyleOp;
use crate::serialize::style_def::build_style_fragment;

/// The four OOXML style families (§17.7.4.18 ST_StyleType).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleType {
    Para,
    Char,
    Table,
    Numbering,
}

/// Run-property subset authorable on a style's `w:rPr`. Bold/italic/underline are
/// the toggle marks; the rest are value properties. Kept deliberately small — a
/// style is a curated handful of properties, not arbitrary direct formatting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyleRunProps {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Font size in half-points (`w:sz` @val; 24 = 12pt).
    pub font_size_half_points: Option<u32>,
    /// Hex RGB color without the leading `#` (`w:color` @val, e.g. "FF0000").
    pub color: Option<String>,
    /// Font family name (`w:rFonts` @ascii/@hAnsi).
    pub font_family: Option<String>,
}

/// Paragraph-property subset authorable on a style's `w:pPr`. Alignment, spacing,
/// and indentation cover the common style-table cases.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyleParaProps {
    /// `w:jc` justification.
    pub alignment: Option<crate::domain::Alignment>,
    /// `w:spacing` @before in twips.
    pub spacing_before: Option<i32>,
    /// `w:spacing` @after in twips.
    pub spacing_after: Option<i32>,
    /// `w:spacing` @line in 240ths of a line (auto rule).
    pub line_spacing: Option<i32>,
    /// `w:ind` @left in twips.
    pub indent_left: Option<i32>,
    /// `w:ind` @right in twips.
    pub indent_right: Option<i32>,
    /// `w:ind` @firstLine (positive) or @hanging (negative) in twips.
    pub indent_first_line: Option<i32>,
}

/// A full style definition: its id, family, optional `basedOn` parent, display
/// name, and the run/paragraph property subsets. The `w:style` fragment is built
/// deterministically from this by [`build_style_fragment`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleDefinition {
    /// `w:styleId` — the programmatic id (`ApplyStyle` references this).
    pub style_id: String,
    pub style_type: StyleType,
    /// `w:basedOn` @val — the parent style id, if any.
    pub based_on: Option<String>,
    /// `w:name` @val — the human-visible style name.
    pub name: String,
    pub run_props: StyleRunProps,
    pub para_props: StyleParaProps,
}

impl StyleDefinition {
    /// Validate the invariants a usable style must satisfy: non-empty id + name.
    /// (Property subsets may all be empty — an empty style that only sets
    /// `basedOn` is legitimate.)
    fn validate(&self, step_index: usize) -> Result<(), EditError> {
        if self.style_id.trim().is_empty() {
            return Err(EditError::StyleDefEmptyId { step_index });
        }
        if self.name.trim().is_empty() {
            return Err(EditError::StyleDefEmptyName {
                style_id: self.style_id.clone(),
                step_index,
            });
        }
        Ok(())
    }
}

/// Apply a `CreateStyle` step: validate the definition, build the fragment, and
/// stage a [`StyleOp::Create`]. Does not touch the body IR.
pub(crate) fn apply_create(
    def: &StyleDefinition,
    step_index: usize,
    style_ops: &mut Vec<StyleOp>,
) -> Result<(), EditError> {
    def.validate(step_index)?;
    let style_xml = build_style_fragment(def);
    style_ops.push(StyleOp::Create {
        style_id: def.style_id.clone(),
        style_xml,
    });
    Ok(())
}

/// Apply a `ModifyStyle` step: validate the definition, check the addressed
/// `style_id` matches `def.style_id` (no silent splice of a mismatched style),
/// build the fragment, and stage a [`StyleOp::Modify`].
pub(crate) fn apply_modify(
    style_id: &str,
    def: &StyleDefinition,
    step_index: usize,
    style_ops: &mut Vec<StyleOp>,
) -> Result<(), EditError> {
    def.validate(step_index)?;
    if style_id != def.style_id {
        return Err(EditError::StyleDefIdMismatch {
            addressed: style_id.to_string(),
            definition: def.style_id.clone(),
            step_index,
        });
    }
    let style_xml = build_style_fragment(def);
    style_ops.push(StyleOp::Modify {
        style_id: def.style_id.clone(),
        style_xml,
    });
    Ok(())
}

/// Apply a `SetDocDefaults` step: stage a [`StyleOp::SetDocDefaults`] that the
/// save path merges into `w:docDefaults/w:rPrDefault/w:rPr`. The one-edit
/// body-text re-skin: body text that inherits from docDefaults picks up the new
/// font/size without touching any individual `w:style`.
///
/// Fails loud (CLAUDE.md "no silent fallbacks") if BOTH `font_family` and
/// `font_size_half_points` are `None` — an op that sets nothing is a no-op the
/// caller did not mean to author.
pub(crate) fn apply_set_doc_defaults(
    font_family: Option<&str>,
    font_size_half_points: Option<u32>,
    step_index: usize,
    style_ops: &mut Vec<StyleOp>,
) -> Result<(), EditError> {
    let font_family = font_family.map(str::to_string);
    if font_family.is_none() && font_size_half_points.is_none() {
        return Err(EditError::DocDefaultsEmpty { step_index });
    }
    style_ops.push(StyleOp::SetDocDefaults {
        font_family,
        font_size_half_points,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Alignment;

    fn def(style_id: &str, name: &str) -> StyleDefinition {
        StyleDefinition {
            style_id: style_id.to_string(),
            style_type: StyleType::Para,
            based_on: None,
            name: name.to_string(),
            run_props: StyleRunProps::default(),
            para_props: StyleParaProps::default(),
        }
    }

    #[test]
    fn create_stages_a_create_op_with_matching_id() {
        let mut ops = Vec::new();
        apply_create(&def("MyStyle", "My Style"), 0, &mut ops).expect("create");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            StyleOp::Create {
                style_id,
                style_xml,
            } => {
                assert_eq!(style_id, "MyStyle");
                let s = String::from_utf8(style_xml.clone()).unwrap();
                assert!(s.contains(r#"w:styleId="MyStyle""#), "{s}");
                assert!(s.contains(r#"w:val="My Style""#), "{s}");
                assert!(s.contains(r#"w:type="paragraph""#), "{s}");
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn empty_id_fails_loud() {
        let mut ops = Vec::new();
        let err = apply_create(&def("  ", "Name"), 0, &mut ops).unwrap_err();
        assert!(
            matches!(err, EditError::StyleDefEmptyId { .. }),
            "got {err:?}"
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn empty_name_fails_loud() {
        let mut ops = Vec::new();
        let err = apply_create(&def("Id", ""), 0, &mut ops).unwrap_err();
        assert!(
            matches!(err, EditError::StyleDefEmptyName { .. }),
            "got {err:?}"
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn modify_id_mismatch_fails_loud() {
        let mut ops = Vec::new();
        let err = apply_modify("Addressed", &def("Different", "N"), 0, &mut ops).unwrap_err();
        assert!(
            matches!(err, EditError::StyleDefIdMismatch { .. }),
            "got {err:?}"
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn props_render_into_rpr_and_ppr() {
        let mut d = def("Heading", "Heading");
        d.based_on = Some("Normal".to_string());
        d.run_props.bold = true;
        d.run_props.font_size_half_points = Some(32);
        d.run_props.color = Some("FF0000".to_string());
        d.para_props.alignment = Some(Alignment::Center);
        d.para_props.spacing_after = Some(120);

        let xml = String::from_utf8(build_style_fragment(&d)).unwrap();
        assert!(
            xml.contains(r#"w:basedOn"#) && xml.contains(r#"w:val="Normal""#),
            "{xml}"
        );
        assert!(xml.contains("<w:b") || xml.contains("w:b/"), "bold: {xml}");
        assert!(xml.contains(r#"w:val="32""#), "size: {xml}");
        assert!(xml.contains(r#"w:val="FF0000""#), "color: {xml}");
        assert!(xml.contains(r#"w:val="center""#), "jc: {xml}");
        assert!(xml.contains(r#"w:after="120""#), "spacing: {xml}");
    }
}
