//! Opinionated document comparison built on the Stemma Word engine.
//!
//! This crate owns correspondence, alignment, comparison presentation, and
//! the compilation of inferred differences into engine-owned native Word
//! operations. The `stemma` crate does not depend on this crate.

pub(crate) use stemma::{edit, runtime, table, tracked_model};

pub(crate) mod word_ir {
    pub(crate) use stemma::TabStopDef;
}

pub(crate) mod domain {
    pub(crate) use crate::model::*;
    pub(crate) use stemma::domain::*;
}

mod compiler;
mod materialize;
mod model;
mod semantic_hash;
mod table_diff;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support;

pub use model::{
    BlockType, ChangeType, CommentPayload, FullDocBlock, FullDocViewResult, HeaderFooterParagraph,
    HeaderFooterPayload, ImageMetadataChange, MoveDirection, StoryPayload, StructuralChange,
};
pub use semantic_hash::block_semantic_hash_for_full_doc_block;

use stemma::api::{Diagnostic, Document, RevisionRecord};
use stemma::domain::RevisionInfo;
use stemma::runtime::{EditSnapshot, ErrorCode, ErrorDetails, ExportOptions, RuntimeError};

/// Compare two accepted document readings using Stemma's single opinionated
/// review presentation.
pub fn diff(base: &Document, target: &Document) -> Result<Document, RuntimeError> {
    Ok(diff_detailed(base, target)?.document)
}

/// Attributed form of [`diff`].
pub fn diff_as(base: &Document, target: &Document, author: &str) -> Result<Document, RuntimeError> {
    Ok(diff_as_detailed(base, target, author)?.document)
}

/// Comparison metadata that belongs to both inputs rather than to the derived
/// redline alone.
///
/// Import diagnostics remain source-labelled: a normalization performed while
/// parsing the target must not disappear merely because the output document is
/// derived from the base package.
pub struct ComparisonResult {
    pub document: Document,
    pub semantic_change_count: usize,
    pub base_diagnostics: Vec<Diagnostic>,
    pub target_diagnostics: Vec<Diagnostic>,
    pub flattened_input_revisions: FlattenedInputRevisions,
}

/// Pending input revisions consumed when comparison projects each input to its
/// accepted reading.
pub struct FlattenedInputRevisions {
    pub base: Vec<RevisionRecord>,
    pub target: Vec<RevisionRecord>,
}

/// Metadata-bearing form of [`diff`]. Product adapters should use this entry
/// point so import normalizations and semantic counts remain observable.
pub fn diff_detailed(base: &Document, target: &Document) -> Result<ComparisonResult, RuntimeError> {
    diff_detailed_with_author(base, target, None)
}

/// Metadata-bearing form of [`diff_as`].
pub fn diff_as_detailed(
    base: &Document,
    target: &Document,
    author: &str,
) -> Result<ComparisonResult, RuntimeError> {
    if author.is_empty() {
        return Err(RuntimeError {
            code: ErrorCode::ValidationFailed,
            message: "diff_as requires a non-empty author; use diff for anonymous discovery"
                .to_string(),
            details: ErrorDetails::default(),
        });
    }
    diff_detailed_with_author(base, target, Some(author.to_string()))
}

fn diff_detailed_with_author(
    base: &Document,
    target: &Document,
    author: Option<String>,
) -> Result<ComparisonResult, RuntimeError> {
    let flattened_input_revisions = FlattenedInputRevisions {
        base: base.revisions(),
        target: target.revisions(),
    };
    let compiled = diff_snapshots(base.snapshot(), target.snapshot(), author, None)?;
    Ok(ComparisonResult {
        document: base.with_derived_snapshot(compiled.snapshot),
        semantic_change_count: compiled.semantic_change_count,
        base_diagnostics: base.diagnostics().to_vec(),
        target_diagnostics: target.diagnostics().to_vec(),
        flattened_input_revisions,
    })
}

/// Build the rich tracked-document read projection used by comparison-aware
/// clients. Media lookup failures are surfaced; a missing asset is not silently
/// converted into an empty projection.
pub fn tracked_document_view(document: &Document) -> Result<FullDocViewResult, RuntimeError> {
    let bytes = document.serialize(&ExportOptions::unchecked())?;
    let archive =
        stemma::docx::DocxArchive::read(&bytes).map_err(stemma::runtime::map_docx_error)?;
    let images = stemma::import::build_image_data_lookup(&archive)?;
    Ok(compiler::build_tracked_document_view(
        &document.snapshot().canonical,
        &images,
    ))
}

/// Compile inferred differences into the engine's native tracked-change and
/// package operations.
struct CompiledComparison {
    snapshot: EditSnapshot,
    semantic_change_count: usize,
}

fn diff_snapshots(
    source: &EditSnapshot,
    target: &EditSnapshot,
    author: Option<String>,
    date: Option<String>,
) -> Result<CompiledComparison, RuntimeError> {
    refuse_quarantined_inputs(&source.canonical, &target.canonical)?;

    // Numbering instance IDs are package-local transport identities. Make
    // semantically different same-ID source/target definitions coexist before
    // correspondence so an ordinary pPrChange can switch the remapped target
    // reference without changing either list definition.
    let target = stemma::runtime::prepare_numbering_transport_for_coexistence(source, target)?;

    let mut accepted_source = (*source.canonical).clone();
    let mut accepted_target = (*target.canonical).clone();
    if !stemma::tracked_model::pending_revision_authors(&accepted_source).is_empty() {
        stemma::tracked_model::accept_all(&mut accepted_source);
    }
    if !stemma::tracked_model::pending_revision_authors(&accepted_target).is_empty() {
        stemma::tracked_model::accept_all(&mut accepted_target);
    }
    refuse_unrevisionable_global_setting_changes(&accepted_source, &accepted_target)?;
    refuse_unrevisionable_default_story_binding_changes(&accepted_source, &accepted_target)?;
    stemma::tracked_model::reconcile_target_comment_ids(&accepted_source, &mut accepted_target)
        .map_err(|error| RuntimeError {
            code: ErrorCode::ValidationFailed,
            message: error.message,
            details: ErrorDetails {
                context: Some(error.context),
                ..ErrorDetails::default()
            },
        })?;

    let document_diff =
        compiler::diff_documents(&accepted_source, &accepted_target).map_err(diff_error)?;
    let semantic_change_count = document_diff.changes.len();
    refuse_unqualified_comment_replacement(&document_diff.changes)?;
    let revision = RevisionInfo {
        revision_id: stemma::runtime::max_revision_id(&source.canonical) + 1,
        identity: 0,
        author,
        date,
        apply_op_id: None,
    };
    let merge = materialize::materialize_change_plan(
        &accepted_source,
        &accepted_target,
        &document_diff,
        &revision,
        &materialize::ComparisonPlanResolver,
    )
    .map_err(|error| RuntimeError {
        code: match error.kind {
            materialize::MergeErrorKind::UnsupportedEdit => ErrorCode::UnsupportedEdit,
            materialize::MergeErrorKind::InvalidPlan => ErrorCode::InternalError,
        },
        message: error.message,
        details: ErrorDetails {
            context: Some(error.context),
            ..ErrorDetails::default()
        },
    })?;
    let mut merged = merge.doc;
    stemma::import::mint_identities(&mut merged);

    let snapshot = stemma::runtime::materialize_compiled_document(source, &target, merged)?;
    Ok(CompiledComparison {
        snapshot,
        semantic_change_count,
    })
}

/// Refuse simultaneous comment-definition insertion and deletion.
///
/// The individually qualified carriers keep an inserted or deleted comment's
/// definition and three story anchors together. Native Word does not preserve
/// that closure when both polarities are composed in one comparison: Accept
/// can remove the inserted definition while Reject can retain an emptied
/// replacement anchor. Until Word demonstrates one atomic replacement form,
/// emit no artifact for this exact carrier composition.
fn refuse_unqualified_comment_replacement(
    changes: &[crate::model::DiffChange],
) -> Result<(), RuntimeError> {
    let inserts_comment = changes
        .iter()
        .any(|change| matches!(change, crate::model::DiffChange::CommentInserted { .. }));
    let deletes_comment = changes
        .iter()
        .any(|change| matches!(change, crate::model::DiffChange::CommentDeleted { .. }));
    if !(inserts_comment && deletes_comment) {
        return Ok(());
    }

    Err(RuntimeError {
        code: ErrorCode::UnsupportedEdit,
        message: "compare refused: simultaneous comment insertion and deletion has no qualified native Word carrier composition"
            .to_string(),
        details: ErrorDetails {
            context: Some(
                "comment definitions and their range/reference anchors must resolve atomically in both native Word terminals"
                    .to_string(),
            ),
            ..ErrorDetails::default()
        },
    })
}

/// Refuse active document-global settings for which Word exposes no tracked
/// Accept/Reject carrier.
///
/// `w:evenAndOddHeaders` selects whether even-page header/footer stories are
/// active. Keeping either input's value in the one physical settings part
/// makes the other native terminal wrong, even when the story happens to be
/// visually blank. The comparison compiler must therefore fail before it
/// constructs an apparently successful redline.
fn refuse_unrevisionable_global_setting_changes(
    base: &stemma::domain::CanonDoc,
    target: &stemma::domain::CanonDoc,
) -> Result<(), RuntimeError> {
    if base.even_and_odd_headers == target.even_and_odd_headers {
        return Ok(());
    }

    Err(RuntimeError {
        code: ErrorCode::UnsupportedEdit,
        message:
            "compare refused: even/odd header selection cannot switch through native Word revisions"
                .to_string(),
        details: ErrorDetails {
            context: Some(format!(
                "base_even_and_odd_headers={:?} target_even_and_odd_headers={:?}; w:evenAndOddHeaders is document-global settings state",
                base.even_and_odd_headers, target.even_and_odd_headers
            )),
            ..ErrorDetails::default()
        },
    })
}

fn refuse_quarantined_inputs(
    base: &stemma::domain::CanonDoc,
    target: &stemma::domain::CanonDoc,
) -> Result<(), RuntimeError> {
    use stemma::domain::{BlockNode, OpaqueKind};

    for (label, document) in [("base", base), ("target", target)] {
        let block_id = document
            .blocks
            .iter()
            .find_map(|tracked| match &tracked.block {
                BlockNode::OpaqueBlock(opaque)
                    if matches!(opaque.kind, OpaqueKind::QuarantinedNestedTracking) =>
                {
                    Some(&opaque.id)
                }
                _ => None,
            });
        if let Some(block_id) = block_id {
            return Err(RuntimeError {
                code: ErrorCode::UnsupportedEdit,
                message: format!(
                    "compare refused: {label} document block '{block_id}' is quarantined \
                     (nested tracked changes in an unsupported shape), so its content \
                     cannot be honestly compared"
                ),
                details: ErrorDetails::default(),
            });
        }
    }
    Ok(())
}

/// Refuse a transition between a synthesized and authored default story.
///
/// The first section's missing default story is represented in the canonical
/// model by a synthesized blank story, but that story has no physical part or
/// relationship. `w:sectPrChange` cannot carry previous header/footer
/// references: its previous-section payload excludes those children. A single
/// redline therefore cannot switch the document between Word's implicit blank
/// default story and an authored default relationship. Tracking the story
/// content alone would leave the authored relationship active in both native
/// Word terminals.
///
/// This is deliberately narrower than all header/footer changes. When both
/// readings already have an authored default story, its relationship can own
/// an ordinary tracked story-content change even when section inheritance
/// places the direct reference on a different `sectPr`. First/even story
/// activation is governed by separate, revision-bearing selectors.
fn refuse_unrevisionable_default_story_binding_changes(
    base: &stemma::domain::CanonDoc,
    target: &stemma::domain::CanonDoc,
) -> Result<(), RuntimeError> {
    use stemma::domain::HeaderFooterKind;

    let base_header_authored = base
        .headers
        .iter()
        .any(|story| story.kind == HeaderFooterKind::Default && !story.synthesized);
    let target_header_authored = target
        .headers
        .iter()
        .any(|story| story.kind == HeaderFooterKind::Default && !story.synthesized);
    let base_footer_authored = base
        .footers
        .iter()
        .any(|story| story.kind == HeaderFooterKind::Default && !story.synthesized);
    let target_footer_authored = target
        .footers
        .iter()
        .any(|story| story.kind == HeaderFooterKind::Default && !story.synthesized);

    let changed_kind = if base_header_authored != target_header_authored {
        Some(("header", base_header_authored, target_header_authored))
    } else if base_footer_authored != target_footer_authored {
        Some(("footer", base_footer_authored, target_footer_authored))
    } else {
        None
    };
    let Some((kind, base_authored, target_authored)) = changed_kind else {
        return Ok(());
    };

    Err(RuntimeError {
        code: ErrorCode::UnsupportedEdit,
        message: format!(
            "compare refused: the default {kind} binding topology cannot switch through native Word revisions"
        ),
        details: ErrorDetails {
            context: Some(format!(
                "base_has_authored_default_{kind}={base_authored} target_has_authored_default_{kind}={target_authored}; w:sectPrChange cannot carry previous header/footer references"
            )),
            ..ErrorDetails::default()
        },
    })
}

fn diff_error(error: compiler::DiffError) -> RuntimeError {
    match error {
        compiler::DiffError::Inference(message) => RuntimeError {
            code: ErrorCode::InternalError,
            message: format!("comparison inference failed: {message}"),
            details: ErrorDetails::default(),
        },
        compiler::DiffError::Unsupported(message) => RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: format!("compare refused: {message}"),
            details: ErrorDetails::default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stemma::api::Resolution;

    fn document_with_text(text: &str) -> Document {
        Document::parse(&docx_with_text(text)).expect("parse test document")
    }

    fn docx_with_text(text: &str) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    fn with_dangling_package_thumbnail(bytes: &[u8]) -> Vec<u8> {
        let mut archive = stemma::docx::DocxArchive::read(bytes).expect("read test package");
        let relationships = String::from_utf8(
            archive
                .get("_rels/.rels")
                .expect("root relationships")
                .to_vec(),
        )
        .expect("test relationships are UTF-8");
        let dangling = r#"<Relationship Id="rIdThumbnail" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail" Target="docProps/thumbnail.jpeg"/>"#;
        archive.upsert(
            "_rels/.rels",
            relationships
                .replacen(
                    "</Relationships>",
                    &format!("{dangling}</Relationships>"),
                    1,
                )
                .into_bytes(),
        );
        archive.write().expect("write test package")
    }

    fn docx_with_numbered_list(glyph: &str, font: &str, durable_id: u32) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let numbering = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w16cid="http://schemas.microsoft.com/office/word/2016/wordml/cid"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="{glyph}"/><w:rPr><w:rFonts w:ascii="{font}" w:hAnsi="{font}"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1" w16cid:durableId="{durable_id}"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document),
                ("word/numbering.xml", numbering.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    fn docx_with_even_and_odd_headers(text: &str, enabled: bool) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );
        let setting = if enabled {
            "<w:evenAndOddHeaders/>"
        } else {
            "<w:evenAndOddHeaders w:val=\"0\"/>"
        };
        let settings = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{setting}</w:settings>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/></Relationships>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document.as_str()),
                ("word/settings.xml", settings.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    fn docx_with_text_and_defaults(text: &str, language: &str) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );
        let styles = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="{language}"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document.as_str()),
                ("word/styles.xml", styles.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    fn docx_with_text_and_style(text: &str, style_property: &str) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Clause"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );
        let styles = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults/><w:style w:type="paragraph" w:styleId="Clause"><w:name w:val="Clause"/><w:rPr><w:{style_property}/></w:rPr></w:style></w:styles>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document.as_str()),
                ("word/styles.xml", styles.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    fn docx_with_default_style(text: &str, style_property: &str) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );
        let styles = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults/><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:{style_property}/></w:rPr></w:style></w:styles>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document.as_str()),
                ("word/styles.xml", styles.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    fn docx_with_default_header(body_text: &str, header_text: &str) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>{body_text}</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr></w:body></w:document>"#
        );
        let header = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>{header_text}</w:t></w:r></w:p></w:hdr>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
        let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;

        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: FileOptions = FileOptions::default();
            for (path, content) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", package_relationships),
                ("word/document.xml", document.as_str()),
                ("word/_rels/document.xml.rels", document_relationships),
                ("word/header1.xml", header.as_str()),
            ] {
                archive.start_file(path, options).expect("start DOCX part");
                archive
                    .write_all(content.as_bytes())
                    .expect("write DOCX part");
            }
            archive.finish().expect("finish DOCX package");
        }
        bytes
    }

    #[test]
    fn diff_reconstructs_both_accepted_readings() {
        let base = document_with_text("Hello world");
        let target = document_with_text("Hello brave world");

        let redline = diff(&base, &target).expect("compare documents");
        let rejected = redline.project(Resolution::RejectAll).expect("reject all");
        let accepted = redline.project(Resolution::AcceptAll).expect("accept all");

        assert_eq!(rejected.to_text(), base.to_text());
        assert_eq!(accepted.to_text(), target.to_text());
    }

    #[test]
    fn detailed_diff_distinguishes_semantic_changes_from_revision_carriers() {
        let base = document_with_text("Hello old world");
        let target = document_with_text("Hello new world");

        let result = diff_detailed(&base, &target).expect("compare documents");

        assert_eq!(result.semantic_change_count, 1);
        assert_eq!(result.document.revisions().len(), 2);
        assert!(result.flattened_input_revisions.base.is_empty());
        assert!(result.flattened_input_revisions.target.is_empty());
    }

    #[test]
    fn detailed_diff_keeps_import_diagnostics_attributed_to_each_input() {
        let clean_base = Document::parse(&docx_with_text("Base")).expect("parse clean base");
        let normalized_target =
            Document::parse(&with_dangling_package_thumbnail(&docx_with_text("Target")))
                .expect("normalize target thumbnail relationship");

        let forward = diff_detailed(&clean_base, &normalized_target).expect("compare forward");
        assert!(forward.base_diagnostics.is_empty());
        assert_eq!(forward.target_diagnostics.len(), 1);
        assert!(
            forward.target_diagnostics[0]
                .message
                .contains("rIdThumbnail")
        );

        let reverse = diff_detailed(&normalized_target, &clean_base).expect("compare reverse");
        assert_eq!(reverse.base_diagnostics.len(), 1);
        assert!(reverse.base_diagnostics[0].message.contains("rIdThumbnail"));
        assert!(reverse.target_diagnostics.is_empty());
    }

    #[test]
    fn diff_switches_colliding_numbering_definitions_through_remapped_num_ids() {
        let base = Document::parse(&docx_with_numbered_list("•", "Symbol", 10))
            .expect("parse source list");
        let target = Document::parse(&docx_with_numbered_list("▪", "Wingdings", 20))
            .expect("parse target list");

        let redline = diff(&base, &target).expect("compare colliding numbering definitions");
        let rejected = redline.project(Resolution::RejectAll).expect("reject all");
        let accepted = redline.project(Resolution::AcceptAll).expect("accept all");
        let paragraph_num_id = |document: &Document| {
            let stemma::domain::BlockNode::Paragraph(paragraph) =
                &document.snapshot().canonical.blocks[0].block
            else {
                panic!("fixture contains one paragraph")
            };
            paragraph
                .numbering
                .as_ref()
                .expect("paragraph remains numbered")
                .num_id
        };
        assert_eq!(paragraph_num_id(&rejected), 1);
        assert_eq!(paragraph_num_id(&accepted), 2);
        assert_eq!(redline.revisions().len(), 1);

        let bytes = redline
            .serialize(&ExportOptions::default())
            .expect("serialize numbering redline");
        let archive = stemma::docx::DocxArchive::read(&bytes).expect("read redline package");
        let document_xml = std::str::from_utf8(
            archive
                .get("word/document.xml")
                .expect("redline has main document part"),
        )
        .expect("document XML is UTF-8");
        let numbering_xml = std::str::from_utf8(
            archive
                .get("word/numbering.xml")
                .expect("redline has numbering part"),
        )
        .expect("numbering XML is UTF-8");
        assert!(document_xml.contains(r#"<w:numId w:val="2""#));
        assert!(document_xml.contains(r#"<w:pPrChange"#));
        assert!(document_xml.contains(r#"<w:numId w:val="1""#));
        assert!(numbering_xml.contains(r#"w:numId="1""#));
        assert!(numbering_xml.contains(r#"w:numId="2""#));
        assert!(numbering_xml.contains("•"));
        assert!(numbering_xml.contains("▪"));
    }

    #[test]
    fn diff_does_not_remap_semantically_equal_numbering_definitions() {
        let base = Document::parse(&docx_with_numbered_list("•", "Symbol", 10))
            .expect("parse source list");
        let target = Document::parse(&docx_with_numbered_list("•", "Symbol", 20))
            .expect("parse target list");

        let redline = diff(&base, &target).expect("generated identity tokens are transport state");
        assert!(
            redline.revisions().is_empty(),
            "a differing Word-generated durableId is not a user-visible list change"
        );
    }

    #[test]
    fn diff_refuses_differing_document_defaults_without_a_native_carrier() {
        let base = Document::parse(&docx_with_text_and_defaults("Old text", "en-US"))
            .expect("parse base defaults");
        let target = Document::parse(&docx_with_text_and_defaults("New text", "en-AU"))
            .expect("parse target defaults");

        let error = match diff(&base, &target) {
            Ok(_) => panic!(
                "one physical styles part cannot switch document defaults on Accept and Reject"
            ),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(error.message.contains("document defaults"), "{error:?}");
    }

    #[test]
    fn diff_allows_identical_document_defaults() {
        let base = Document::parse(&docx_with_text_and_defaults("Old text", "en-US"))
            .expect("parse base defaults");
        let target = Document::parse(&docx_with_text_and_defaults("New text", "en-US"))
            .expect("parse target defaults");

        diff(&base, &target).expect("identical global defaults need no switching carrier");
    }

    #[test]
    fn diff_refuses_differing_even_and_odd_header_selection() {
        let base = Document::parse(&docx_with_even_and_odd_headers("Old text", true))
            .expect("parse base settings");
        let target = Document::parse(&docx_with_even_and_odd_headers("New text", false))
            .expect("parse target settings");

        let error = match diff(&base, &target) {
            Ok(_) => panic!("one physical settings part cannot switch even-page story selection"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(
            error.message.contains("even/odd header selection"),
            "{error:?}"
        );
    }

    #[test]
    fn diff_allows_identical_even_and_odd_header_selection() {
        let base = Document::parse(&docx_with_even_and_odd_headers("Old text", true))
            .expect("parse base settings");
        let target = Document::parse(&docx_with_even_and_odd_headers("New text", true))
            .expect("parse target settings");

        diff(&base, &target).expect("identical global selector needs no switching carrier");
    }

    #[test]
    fn diff_refuses_an_active_same_id_style_collision() {
        let base =
            Document::parse(&docx_with_text_and_style("Old text", "b")).expect("parse base style");
        let target = Document::parse(&docx_with_text_and_style("New text", "i"))
            .expect("parse target style");

        let error = match diff(&base, &target) {
            Ok(_) => panic!("one physical style definition cannot serve both native terminals"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(error.message.contains("active style 'Clause'"), "{error:?}");
    }

    #[test]
    fn diff_refuses_a_differing_default_style_without_an_explicit_reference() {
        let base = Document::parse(&docx_with_default_style("Old text", "b"))
            .expect("parse base default style");
        let target = Document::parse(&docx_with_default_style("New text", "i"))
            .expect("parse target default style");

        let error = match diff(&base, &target) {
            Ok(_) => panic!("a styleless paragraph still consumes the default style"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(error.message.contains("active style 'Normal'"), "{error:?}");
    }

    #[test]
    fn simultaneous_comment_insertion_and_deletion_refuses() {
        use crate::model::DiffChange;

        let changes = vec![
            DiffChange::CommentDeleted {
                id: "source-comment".to_string(),
                content_hash: "source".to_string(),
                blocks: Vec::new(),
            },
            DiffChange::CommentInserted {
                id: "target-comment".to_string(),
                content_hash: "target".to_string(),
                blocks: Vec::new(),
            },
        ];

        let error = refuse_unqualified_comment_replacement(&changes)
            .expect_err("unqualified comment replacement must refuse");
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(
            error
                .message
                .contains("simultaneous comment insertion and deletion")
        );
    }

    #[test]
    fn diff_refuses_a_styles_part_presence_transition() {
        let base = document_with_text("Old text");
        let target = Document::parse(&docx_with_text_and_defaults("New text", "en-US"))
            .expect("parse target defaults");

        let error = match diff(&base, &target) {
            Ok(_) => panic!("one physical package cannot switch styles.xml presence"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(error.message.contains("styles.xml presence"), "{error:?}");
    }

    #[test]
    fn diff_as_attributes_serialized_revisions() {
        let base = document_with_text("Hello world");
        let target = document_with_text("Hello brave world");

        let redline = diff_as(&base, &target, "Reviewer").expect("compare with attribution");
        let bytes = redline
            .serialize(&ExportOptions::default())
            .expect("serialize redline");
        let reparsed = Document::parse(&bytes).expect("reparse redline");

        assert!(!reparsed.revisions().is_empty());
        assert!(
            reparsed
                .revisions()
                .iter()
                .all(|revision| revision.author.as_deref() == Some("Reviewer"))
        );
    }

    #[test]
    fn diff_as_refuses_empty_author() {
        let base = document_with_text("Hello world");
        let target = document_with_text("Hello brave world");

        let error = match diff_as(&base, &target, "") {
            Ok(_) => panic!("empty attribution must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert!(error.message.contains("author"));
    }

    #[test]
    fn diff_refuses_added_body_default_header_binding() {
        let base = document_with_text("Same body");
        let target = Document::parse(&docx_with_default_header("Same body", "Target header"))
            .expect("parse target with default header");

        let error = match diff(&base, &target) {
            Ok(_) => {
                panic!("a final-section default header relationship has no native dual terminal")
            }
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedEdit);
        assert!(error.message.contains("default header binding"));
        assert!(error.message.contains("cannot switch"));
    }

    #[test]
    fn diff_allows_default_header_content_change_with_stable_binding() {
        let base = Document::parse(&docx_with_default_header("Same body", "Old header"))
            .expect("parse base with default header");
        let target = Document::parse(&docx_with_default_header("Same body", "New header"))
            .expect("parse target with default header");

        diff(&base, &target).expect("stable header binding can carry a tracked story edit");
    }

    #[test]
    fn default_binding_guard_ignores_section_count_when_no_binding_exists() {
        use std::sync::Arc;

        let base = document_with_text("Same body");
        let mut target = base.snapshot().clone();
        let target_doc = Arc::make_mut(&mut target.canonical);
        let stemma::domain::BlockNode::Paragraph(paragraph) =
            &mut target_doc.blocks.first_mut().expect("body paragraph").block
        else {
            panic!("fixture must begin with a paragraph");
        };
        paragraph.section_properties = Some(stemma::domain::SectionProperties::default());

        refuse_unrevisionable_default_story_binding_changes(
            &base.snapshot().canonical,
            &target.canonical,
        )
        .expect("sections without authored default bindings do not create a binding transition");
    }
}
