//! Integration tests for the SAFE US vs Canada document comparison.
//!
//! This test validates that our diff algorithm correctly identifies the key differences
//! between the Y Combinator SAFE (Simple Agreement for Future Equity) document
//! for US jurisdiction vs the Canadian adaptation.
//!
//! ## Semantic Differences (locked in by tests):
//!
//! ### 1. New Canadian legends/disclaimers
//! - "Please seek advice from an attorney licensed in Canada..."
//! - 4 months + 1 day Canadian resale restriction legend
//! - "United States of America Securities Act" wording tweak
//!
//! ### 2. Jurisdiction + currency
//! - "[State of Incorporation]" → "[Canada / Applicable Province]"
//! - `$` → `US$` for Purchase Amount and Valuation Cap
//!
//! ### 3. Terminology: "stock" → "shares"
//! - Capital Stock → Capital Shares
//! - Common Stock → Common Shares
//! - Preferred Stock → Preferred Shares
//!
//! ### 4. Definitions materially revised
//! - Change of Control rewritten (Canadian-style + "Group Companies")
//! - Direct Listing expanded (Form F-1, non-U.S. exchanges)
//! - "Group Companies" definition added
//! - IPO definition updated (any securities exchange)
//! - Explicit Common Shares / Preferred Shares definitions
//!
//! ### 5. Added/updated representations
//! - Company rep: "private issuer" (Ontario/NI 45-106), not a reporting issuer
//! - Investor rep: accredited investor broadened to U.S. and/or Canadian
//! - Investor rep: consent to disclosure to Canadian securities regulators
//!
//! ### 6. Miscellaneous / notices / governing law
//! - Notice: "internationally recognized overnight courier"
//! - Notice: "Canadian or U.S. mail"
//! - Governing law: Province + federal laws of Canada
//! - Currency clarification: "$" or "Dollars" means USD

use std::fs;
use std::sync::LazyLock;
use stemma_diff::test_support::DiffChange;
#[allow(unused_imports)]
use stemma_diff::test_support::{DocumentComparisonExt as _, RuntimeComparisonExt as _};

use stemma_diff::test_support::DocumentDiff;

use stemma::{
    BlockNode, DocxRuntime, FieldKind, InlineChange, InlineNode, OpaqueKind, SimpleRuntime,
    TransactionMeta,
};

// =============================================================================
// Test helpers
// =============================================================================

fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => out.push_str(&t.text),
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
            InlineNode::Decoration(_) => {} // Zero-width
            InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {} // Zero-width
        }
    }
    out
}

fn block_text(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => {
            let inlines = p.all_inlines_owned();
            extract_inline_text(&inlines)
        }
        _ => String::new(),
    }
}

fn footer_refs_from_section_properties(
    paragraph: &stemma::ParagraphNode,
) -> Option<Vec<(String, String)>> {
    paragraph.section_properties.as_ref().map(|sp| {
        sp.footer_refs
            .iter()
            .map(|r| (format!("{:?}", r.kind).to_lowercase(), r.part_path.clone()))
            .collect()
    })
}

fn footer_story<'a>(doc: &'a stemma::CanonDoc, part_name: &str) -> &'a stemma::FooterStory {
    doc.footers
        .iter()
        .find(|footer| footer.part_name == part_name)
        .unwrap_or_else(|| panic!("should find footer story {part_name}"))
}

fn footer_page_number_paragraph<'a>(
    doc: &'a stemma::CanonDoc,
    part_name: &str,
    page_text: &str,
) -> &'a stemma::ParagraphNode {
    footer_story(doc, part_name)
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            stemma::BlockNode::Paragraph(p) if block_text(&tracked.block).contains(page_text) => {
                Some(p)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("should find footer page number paragraph in {part_name}"))
}

/// Aggregated diff content for assertions.
struct DiffSummary {
    /// Text from modified blocks (old/before version)
    modified_old_texts: Vec<String>,
    /// Text from modified blocks (new/after version)
    modified_new_texts: Vec<String>,
    /// Text from deleted blocks
    deleted_texts: Vec<String>,
    /// Text from inserted blocks
    inserted_texts: Vec<String>,
    /// All inline deletions within modified blocks
    all_inline_deletions: Vec<String>,
    /// All inline insertions within modified blocks
    all_inline_insertions: Vec<String>,
}

impl DiffSummary {
    /// Check if any "after" content (modified_new or inserted) contains the text.
    fn after_contains(&self, needle: &str) -> bool {
        self.modified_new_texts.iter().any(|t| t.contains(needle))
            || self.inserted_texts.iter().any(|t| t.contains(needle))
    }

    /// Check if any "before" content (modified_old or deleted) contains the text.
    fn before_contains(&self, needle: &str) -> bool {
        self.modified_old_texts.iter().any(|t| t.contains(needle))
            || self.deleted_texts.iter().any(|t| t.contains(needle))
    }
}

fn summarize_diff(changes: &[DiffChange]) -> DiffSummary {
    let mut summary = DiffSummary {
        modified_old_texts: Vec::new(),
        modified_new_texts: Vec::new(),
        deleted_texts: Vec::new(),
        inserted_texts: Vec::new(),
        all_inline_deletions: Vec::new(),
        all_inline_insertions: Vec::new(),
    };

    for change in changes {
        match change {
            DiffChange::BlockModified {
                old_text,
                new_text,
                inline_changes,
                ..
            } => {
                summary.modified_old_texts.push(old_text.clone());
                summary.modified_new_texts.push(new_text.clone());
                for ic in inline_changes {
                    match ic {
                        InlineChange::Deleted { text, .. } => {
                            summary.all_inline_deletions.push(text.clone());
                        }
                        InlineChange::Inserted { text, .. } => {
                            summary.all_inline_insertions.push(text.clone());
                        }
                        InlineChange::Unchanged { .. } => {}
                        InlineChange::Opaque {
                            segment_type: stemma::InlineChangeSegmentType::Delete,
                            text: Some(text),
                            ..
                        } => summary.all_inline_deletions.push(text.clone()),
                        InlineChange::Opaque {
                            segment_type: stemma::InlineChangeSegmentType::Insert,
                            text: Some(text),
                            ..
                        } => summary.all_inline_insertions.push(text.clone()),
                        InlineChange::Opaque { .. } => {}
                    }
                }
            }
            DiffChange::BlockDeleted { old_text, .. } => {
                summary.deleted_texts.push(old_text.clone());
            }
            DiffChange::BlockInserted { block, .. } => {
                summary.inserted_texts.push(block_text(block));
            }
            DiffChange::TableStructureChanged { .. } => {
                // Table structure changes are not included in this summary
            }
            // Story-level changes are not included in this summary
            _ => {}
        }
    }

    summary
}

/// Cached diff result shared across all tests that only need the diff summary.
/// Computed once on first access, then reused by all 35+ tests.
static CACHED_DIFF: LazyLock<(DiffSummary, DocumentDiff)> = LazyLock::new(|| {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff should succeed");

    let summary = summarize_diff(&diff.changes);
    (summary, diff)
});

/// Cached imports for tests that need access to the runtime and imported documents.
struct CachedImports {
    /// Kept alive so doc handles remain valid; not read directly.
    #[allow(dead_code)]
    runtime: SimpleRuntime,
    import_before: stemma::ImportResult,
    import_after: stemma::ImportResult,
}

static CACHED_IMPORTS: LazyLock<CachedImports> = LazyLock::new(|| {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    CachedImports {
        runtime,
        import_before,
        import_after,
    }
});

// =============================================================================
// 1. New Canadian legends/disclaimers up front
// =============================================================================

/// Detects: "Please seek advice from an attorney licensed in Canada..."
#[test]
fn detects_canadian_legal_disclaimer() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("attorney licensed in Canada"),
        "should detect Canadian legal disclaimer"
    );
}

/// Detects: 4 months + 1 day Canadian resale restriction legend
#[test]
fn detects_four_month_resale_restriction() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("4 MONTHS AND A DAY"),
        "should detect 4 months + 1 day resale restriction"
    );
}

/// Detects: "REPORTING ISSUER" in Canadian securities legend
#[test]
fn detects_reporting_issuer_legend() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("REPORTING ISSUER"),
        "should detect REPORTING ISSUER in Canadian legend"
    );
}

/// Detects: "SECURITIES LEGISLATION" notice
#[test]
fn detects_securities_legislation_notice() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("SECURITIES LEGISLATION"),
        "should detect SECURITIES LEGISLATION notice"
    );
}

/// Detects: "United States of America Securities Act" wording
#[test]
fn detects_united_states_of_america_securities_act() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("UNITED STATES OF AMERICA SECURITIES ACT"),
        "should detect expanded 'United States of America Securities Act' wording"
    );
}

// =============================================================================
// 2. Jurisdiction + currency
// =============================================================================

/// Detects: "[Canada / Applicable Province]" jurisdiction
#[test]
fn detects_canada_applicable_province() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Canada / Applicable Province")
            || summary.after_contains("Canada") && summary.after_contains("Province"),
        "should detect Canadian jurisdiction placeholder"
    );
}

/// Detects: "[State of Incorporation]" in US version
#[test]
fn detects_state_of_incorporation_removed() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.before_contains("State of Incorporation"),
        "should detect '[State of Incorporation]' in US version"
    );
}

/// Detects: `US$` currency notation in Canadian version
#[test]
fn detects_usd_currency_notation() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("US$"),
        "should detect 'US$' currency notation"
    );
}

/// Detects: Currency clarification clause ("$" or "Dollars" means USD)
#[test]
fn detects_dollar_clarification_clause() {
    let (summary, _) = &*CACHED_DIFF;
    // "all references to "$" or "Dollars" refers to lawful currency of the United States"
    let has_clause = summary.after_contains("lawful currency of the United States")
        || summary.after_contains("Dollars");
    assert!(has_clause, "should detect USD clarification clause");
}

// =============================================================================
// 3. Terminology: "stock" → "shares"
// =============================================================================

/// Detects: "Capital Stock" → "Capital Shares"
#[test]
fn detects_capital_stock_to_capital_shares() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.before_contains("Capital Stock"),
        "should detect 'Capital Stock' in US version"
    );
    assert!(
        summary.after_contains("Capital Shares"),
        "should detect 'Capital Shares' in Canadian version"
    );
}

/// Detects: "Common Stock" → "Common Shares"
#[test]
fn detects_common_stock_to_common_shares() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.before_contains("Common Stock"),
        "should detect 'Common Stock' in US version"
    );
    assert!(
        summary.after_contains("Common Shares"),
        "should detect 'Common Shares' in Canadian version"
    );
}

/// Detects: "Preferred Stock" → "Preferred Shares"
#[test]
fn detects_preferred_stock_to_preferred_shares() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.before_contains("Preferred Stock"),
        "should detect 'Preferred Stock' in US version"
    );
    assert!(
        summary.after_contains("Preferred Shares"),
        "should detect 'Preferred Shares' in Canadian version"
    );
}

/// Detects: "stockholder" → "shareholder" terminology
#[test]
fn detects_stockholder_to_shareholder() {
    let (summary, _) = &*CACHED_DIFF;
    // Note: may appear as "stockholder" or "stockholders"
    let has_shareholder = summary.after_contains("shareholder");
    assert!(has_shareholder, "should detect 'shareholder' terminology");
}

// =============================================================================
// 4. Definitions materially revised
// =============================================================================

/// Detects: "Group Companies" definition added
#[test]
fn detects_group_companies_definition() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Group Companies"),
        "should detect 'Group Companies' definition"
    );
}

/// Detects: "Common Shares" explicit definition added
#[test]
fn detects_common_shares_definition() {
    let (summary, _) = &*CACHED_DIFF;
    // Canadian version adds: "Common Shares" means the Company's common shares or ordinary shares...
    assert!(
        summary.after_contains("ordinary shares"),
        "should detect explicit Common Shares definition with 'ordinary shares'"
    );
}

/// Detects: "Preferred Shares" explicit definition added
#[test]
fn detects_preferred_shares_definition() {
    let (summary, _) = &*CACHED_DIFF;
    // Canadian version adds: "Preferred Shares" means the Company's preferred shares or preference shares...
    assert!(
        summary.after_contains("preference shares"),
        "should detect explicit Preferred Shares definition with 'preference shares'"
    );
}

/// Detects: Direct Listing expanded to include Form F-1
#[test]
fn detects_direct_listing_form_f1() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Form F-1"),
        "should detect Form F-1 in Direct Listing definition"
    );
}

/// Detects: Direct Listing expanded to include non-U.S. exchanges
#[test]
fn detects_direct_listing_non_us_exchanges() {
    let (summary, _) = &*CACHED_DIFF;
    // "any analogous listing not involving any underwritten offering of securities in any exchange
    // located in a jurisdiction other than the United States"
    assert!(
        summary.after_contains("jurisdiction other than the United States")
            || summary.after_contains("other than the United States"),
        "should detect non-U.S. exchange provision in Direct Listing"
    );
}

/// Detects: IPO definition updated to "any securities exchange"
#[test]
fn detects_ipo_any_securities_exchange() {
    let (summary, _) = &*CACHED_DIFF;
    // Canadian: "listing of such Common Shares on any securities exchange"
    assert!(
        summary.after_contains("any securities exchange"),
        "should detect 'any securities exchange' in IPO definition"
    );
}

/// Detects: Change of Control includes "amalgamation" (Canadian term)
#[test]
fn detects_change_of_control_amalgamation() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("amalgamation"),
        "should detect 'amalgamation' in Change of Control definition"
    );
}

/// Detects: Change of Control includes "scheme of arrangement"
#[test]
fn detects_change_of_control_scheme_of_arrangement() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("scheme of arrangement"),
        "should detect 'scheme of arrangement' in Change of Control definition"
    );
}

// =============================================================================
// 5. Added/updated representations
// =============================================================================

/// Detects: Company rep - "private issuer" qualification
#[test]
fn detects_private_issuer_rep() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("private issuer"),
        "should detect 'private issuer' company representation"
    );
}

/// Detects: Company rep - NI 45-106 reference (Canadian securities regulation)
#[test]
fn detects_ni_45_106_reference() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("45-106"),
        "should detect NI 45-106 reference"
    );
}

/// Detects: Company rep - Ontario Securities Act reference
#[test]
fn detects_ontario_securities_act_reference() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Securities Act (Ontario)"),
        "should detect Ontario Securities Act reference"
    );
}

/// Detects: Investor rep - accredited investor under Canadian securities laws
#[test]
fn detects_canadian_accredited_investor() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Canadian securities laws")
            || summary.after_contains("applicable Canadian"),
        "should detect Canadian accredited investor provision"
    );
}

/// Detects: Investor rep - consent to disclosure to Canadian regulators
#[test]
fn detects_consent_to_disclosure() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Canadian securities regulators")
            || summary.after_contains("consents to and authorizes"),
        "should detect consent to disclosure provision"
    );
}

/// Detects: Investor rep - "provincial securities laws" reference
#[test]
fn detects_provincial_securities_laws() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("provincial"),
        "should detect 'provincial' securities laws reference"
    );
}

// =============================================================================
// 6. Miscellaneous / notices / governing law
// =============================================================================

/// Detects: Notice delivery via "internationally recognized overnight courier"
#[test]
fn detects_international_courier_notice() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("internationally recognized"),
        "should detect 'internationally recognized overnight courier'"
    );
}

/// Detects: Notice delivery via "Canadian or U.S. mail"
#[test]
fn detects_canadian_us_mail_notice() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("Canadian or U.S. mail")
            || summary.after_contains("Canadian") && summary.after_contains("mail"),
        "should detect Canadian or U.S. mail notice provision"
    );
}

/// Detects: Governing law - "federal laws of Canada"
#[test]
fn detects_federal_laws_of_canada() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("federal laws of Canada"),
        "should detect 'federal laws of Canada' governing law"
    );
}

/// Detects: Governing law - Province reference in Canadian version
#[test]
fn detects_province_governing_law() {
    let (summary, _) = &*CACHED_DIFF;
    // The governing law clause mentions "Province" - may be "Province of [___]" or just "Province"
    // Also check for "province" lowercase as it appears in jurisdiction placeholder
    let has_province = summary.after_contains("Province")
        || summary.after_contains("province")
        || summary.after_contains("Applicable Province");
    assert!(
        has_province,
        "should detect Province reference in governing law"
    );
}

/// Detects: Non-exclusive jurisdiction of Courts
#[test]
fn detects_non_exclusive_jurisdiction() {
    let (summary, _) = &*CACHED_DIFF;
    assert!(
        summary.after_contains("non-exclusive jurisdiction"),
        "should detect 'non-exclusive jurisdiction' clause"
    );
}

// =============================================================================
// Summary and structural tests
// =============================================================================

/// Validates that the diff produces a substantial number of changes.
#[test]
fn diff_produces_substantial_changes() {
    let (_, diff) = &*CACHED_DIFF;

    // Should have many changes - these are substantially different documents
    assert!(
        diff.changes.len() > 50,
        "expected >50 changes for US vs Canada SAFE, got {}",
        diff.changes.len()
    );

    // Should have all three types of changes
    let has_modified = diff
        .changes
        .iter()
        .any(|c| matches!(c, DiffChange::BlockModified { .. }));
    let has_deleted = diff
        .changes
        .iter()
        .any(|c| matches!(c, DiffChange::BlockDeleted { .. }));
    let has_inserted = diff
        .changes
        .iter()
        .any(|c| matches!(c, DiffChange::BlockInserted { .. }));

    assert!(has_modified, "should have modified blocks");
    assert!(has_deleted, "should have deleted blocks");
    assert!(has_inserted, "should have inserted blocks");
}

/// Validates document structure (paragraph counts).
#[test]
fn document_structure_is_valid() {
    let imports = &*CACHED_IMPORTS;

    let before_para_count = imports
        .import_before
        .canonical
        .blocks
        .iter()
        .filter(|b| matches!(&b.block, BlockNode::Paragraph(_)))
        .count();

    let after_para_count = imports
        .import_after
        .canonical
        .blocks
        .iter()
        .filter(|b| matches!(&b.block, BlockNode::Paragraph(_)))
        .count();

    // Both documents should have substantial content
    assert!(
        before_para_count > 50,
        "US SAFE should have >50 paragraphs, got {before_para_count}"
    );
    assert!(
        after_para_count > 50,
        "Canadian SAFE should have >50 paragraphs, got {after_para_count}"
    );

    // Canadian version has additional content
    assert!(
        after_para_count >= before_para_count,
        "Canadian SAFE should have >= paragraphs ({after_para_count} vs {before_para_count})"
    );
}

/// The natural SAFE pair has differing active document defaults, for which a
/// native dual-terminal carrier has not been qualified. The refusal must name
/// that real boundary rather than an unrelated downstream population guard.
#[test]
fn redline_refuses_differing_active_document_defaults() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let meta = TransactionMeta {
        author: "safe_us_vs_canada".to_string(),
        reason: Some("SAFE US vs Canada comparison".to_string()),
        timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
    };

    let error = runtime
        .diff_and_redline(&import_before.doc_handle, &import_after.doc_handle, meta)
        .expect_err("unqualified document-default difference must refuse");

    assert_eq!(error.code, stemma::ErrorCode::UnsupportedEdit);
    assert!(
        error.message.contains("document defaults"),
        "refusal should identify the active document-default boundary: {error:?}"
    );
}

#[test]
fn import_preserves_clause_body_run_boundaries() {
    let imports = &*CACHED_IMPORTS;
    let paragraph = imports
        .import_after
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p)
                if extract_inline_text(&p.all_inlines_owned())
                    .contains("The execution, delivery and performance") =>
            {
                Some(p)
            }
            _ => None,
        })
        .expect("should find Canada clause paragraph");

    let text_runs: Vec<String> = paragraph
        .segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(
        text_runs.first().map(String::as_str),
        Some("The execution, "),
        "target import should preserve the first body run boundary after the stripped clause prefix"
    );
    assert_eq!(
        text_runs.get(1).map(String::as_str),
        Some("delivery"),
        "target import should preserve the second body run boundary in the Canada clause paragraph"
    );
}

#[test]
fn import_preserves_explicit_first_line_indent_on_tabbed_clause() {
    let imports = &*CACHED_IMPORTS;
    let paragraph = imports
        .import_after
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p)
                if extract_inline_text(&p.all_inlines_owned()).contains("private issuer") =>
            {
                Some(p)
            }
            _ => None,
        })
        .expect("should find imported Canada private issuer clause");

    let indent = paragraph
        .indent
        .as_ref()
        .expect("private issuer clause should keep indentation");
    assert_eq!(
        indent.left,
        Some(-720),
        "fixture expectation changed: Canada private issuer clause left indent"
    );
    assert_eq!(
        indent.effective_first_line_twips,
        Some(720),
        "import should preserve explicit firstLine on tabbed literal-prefix clauses"
    );
}

#[test]
fn diff_target_tax_list_paragraph_keeps_structural_numbering() {
    let (_, diff) = &*CACHED_DIFF;
    let new_para = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified { new_block, .. } => match new_block {
                BlockNode::Paragraph(p)
                    if extract_inline_text(&p.all_inlines_owned())
                        .contains("United States federal and state income tax purposes") =>
                {
                    Some(p)
                }
                _ => None,
            },
            DiffChange::BlockInserted { block, .. } => match block {
                BlockNode::Paragraph(p)
                    if extract_inline_text(&p.all_inlines_owned())
                        .contains("United States federal and state income tax purposes") =>
                {
                    Some(p)
                }
                _ => None,
            },
            _ => None,
        })
        .expect("should find Canada tax paragraph in diff target side");

    assert!(
        new_para.numbering.is_some(),
        "diff target paragraph should keep structural numbering from the imported target block"
    );
    assert!(
        new_para.literal_prefix.is_none(),
        "diff target paragraph should not invent a literal_prefix for the structural list item"
    );
}

#[test]
fn diff_pairs_tax_list_paragraph_to_baked_prefix_base_clause() {
    let (_, diff) = &*CACHED_DIFF;
    let change = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified {
                old_text, new_text, ..
            } if new_text.contains("United States federal and state income tax purposes") => {
                Some((old_text, new_text))
            }
            _ => None,
        })
        .expect("should find BlockModified for Canada tax paragraph");

    assert!(
        change.0.contains("characterized as stock")
            && !change.0.contains("“stock,”")
            && change.1.contains("“stock,”"),
        "the Canada tax paragraph should diff against the baked-prefix US clause text, not a signature placeholder.\nold={:?}\nnew={:?}",
        change.0,
        change.1
    );
}

#[test]
fn import_preserves_target_first_section_footer_refs() {
    let imports = &*CACHED_IMPORTS;
    let footer_refs = imports
        .import_after
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) => footer_refs_from_section_properties(p),
            _ => None,
        })
        .expect("target import should have a first section-break paragraph");

    assert_eq!(
        footer_refs,
        vec![
            ("even".to_string(), "footer1.xml".to_string()),
            ("default".to_string(), "footer2.xml".to_string()),
            ("first".to_string(), "footer3.xml".to_string()),
        ],
        "target import should preserve the first section footer refs from after.docx"
    );
}

#[test]
fn import_preserves_even_footer_story_for_first_section() {
    let imports = &*CACHED_IMPORTS;
    let footer_stories: Vec<(String, String)> = imports
        .import_after
        .canonical
        .footers
        .iter()
        .map(|footer| {
            (
                format!("{:?}", footer.kind).to_lowercase(),
                footer.part_name.clone(),
            )
        })
        .collect();

    assert!(
        footer_stories
            .iter()
            .any(|(kind, part_name)| kind == "even" && part_name == "footer1.xml"),
        "target import should keep the referenced even footer story for the first section; got {footer_stories:?}"
    );
}

#[test]
fn diff_preserves_target_first_section_footer_refs_in_new_block() {
    let (_, diff) = &*CACHED_DIFF;
    let footer_refs = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified {
                new_block: BlockNode::Paragraph(p),
                ..
            } => footer_refs_from_section_properties(p),
            DiffChange::BlockInserted {
                block: BlockNode::Paragraph(p),
                ..
            } => footer_refs_from_section_properties(p),
            _ => None,
        })
        .expect("diff should preserve the target first section-break paragraph");

    assert_eq!(
        footer_refs,
        vec![
            ("even".to_string(), "footer1.xml".to_string()),
            ("default".to_string(), "footer2.xml".to_string()),
            ("first".to_string(), "footer3.xml".to_string()),
        ],
        "diff target side should preserve the first section footer refs"
    );
}

#[test]
fn import_preserves_target_body_section_footer_refs() {
    let imports = &*CACHED_IMPORTS;
    let footer_refs: Vec<(String, String)> = imports
        .import_after
        .canonical
        .body_section_properties
        .as_ref()
        .expect("target import should keep body section properties")
        .footer_refs
        .iter()
        .map(|r| (format!("{:?}", r.kind).to_lowercase(), r.part_path.clone()))
        .collect();

    assert_eq!(
        footer_refs,
        vec![
            ("default".to_string(), "footer4.xml".to_string()),
            ("first".to_string(), "footer5.xml".to_string()),
            ("even".to_string(), "footer1.xml".to_string()),
        ],
        "target import should preserve the body-level section footer refs from after.docx"
    );
}

#[test]
fn diff_preserves_clause_body_run_boundaries_in_target_block() {
    let (_, diff) = &*CACHED_DIFF;
    let new_para = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified { new_block, .. } => match new_block {
                BlockNode::Paragraph(p)
                    if extract_inline_text(&p.all_inlines_owned())
                        .contains("The execution, delivery and performance") =>
                {
                    Some(p)
                }
                _ => None,
            },
            _ => None,
        })
        .expect("should find Canada clause BlockModified target paragraph");

    let text_runs: Vec<String> = new_para
        .segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(
        text_runs.first().map(String::as_str),
        Some("The execution, "),
        "diff target block should preserve the first body run boundary after the stripped clause prefix"
    );
    assert_eq!(
        text_runs.get(1).map(String::as_str),
        Some("delivery"),
        "diff target block should preserve the second body run boundary in the Canada clause paragraph"
    );
}

/// Summary test that outputs diff statistics.
#[test]
fn diff_summary_statistics() {
    let (_, diff) = &*CACHED_DIFF;

    let modified_count = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
        .count();
    let deleted_count = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
        .count();
    let inserted_count = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
        .count();
    let story_change_count = diff
        .changes
        .iter()
        .filter(|c| {
            matches!(
                c,
                DiffChange::HeaderModified { .. }
                    | DiffChange::HeaderDeleted { .. }
                    | DiffChange::HeaderInserted { .. }
                    | DiffChange::FooterModified { .. }
                    | DiffChange::FooterDeleted { .. }
                    | DiffChange::FooterInserted { .. }
                    | DiffChange::FootnoteModified { .. }
                    | DiffChange::FootnoteDeleted { .. }
                    | DiffChange::FootnoteInserted { .. }
                    | DiffChange::EndnoteModified { .. }
                    | DiffChange::EndnoteDeleted { .. }
                    | DiffChange::EndnoteInserted { .. }
                    | DiffChange::CommentModified { .. }
                    | DiffChange::CommentDeleted { .. }
                    | DiffChange::CommentInserted { .. }
            )
        })
        .count();

    println!("\n=== SAFE US vs Canada Diff Statistics ===");
    println!("Total changes: {}", diff.changes.len());
    println!("  - Modified: {modified_count}");
    println!("  - Deleted:  {deleted_count}");
    println!("  - Inserted: {inserted_count}");
    println!("  - Story-level: {story_change_count}");

    assert!(!diff.changes.is_empty(), "should have changes");
    assert_eq!(
        modified_count + deleted_count + inserted_count + story_change_count,
        diff.changes.len(),
        "change counts should sum correctly"
    );
}

/// Regression test: whitespace must be preserved between words.
/// Previously, xmltree 0.10.3 discarded whitespace-only text nodes,
/// causing "THAT in" to become "THATin".
#[test]
fn whitespace_preserved_between_words() {
    let (summary, _) = &*CACHED_DIFF;

    // Check that no text has concatenated words due to lost whitespace
    let all_texts: Vec<&str> = summary
        .modified_old_texts
        .iter()
        .chain(&summary.modified_new_texts)
        .chain(&summary.deleted_texts)
        .chain(&summary.inserted_texts)
        .map(|s| s.as_str())
        .collect();

    for text in &all_texts {
        // Common patterns that would indicate lost whitespace
        assert!(
            !text.contains("THATin"),
            "space lost between 'THAT' and 'in': {text:?}"
        );
        assert!(
            !text.contains("withthe"),
            "space lost between 'with' and 'the': {text:?}"
        );
        assert!(
            !text.contains("forthe"),
            "space lost between 'for' and 'the': {text:?}"
        );
        assert!(
            !text.contains("ofthe"),
            "space lost between 'of' and 'the': {text:?}"
        );
    }

    // Positive check: verify proper spacing exists
    let has_that_in = all_texts
        .iter()
        .any(|t| t.contains("THAT in") || t.contains("that in"));
    assert!(
        has_that_in,
        "should find 'THAT in' or 'that in' with proper spacing in document"
    );
}

/// Diagnostic test to find opaque inlines in the documents.
#[test]
fn inspect_opaque_inlines() {
    let imports = &*CACHED_IMPORTS;

    println!("\n=== Opaque Inlines in BEFORE document ===");
    for block in &imports.import_before.canonical.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline {
                    let inlines = p.all_inlines_owned();
                    let text = extract_inline_text(&inlines);
                    let preview: String = text.chars().take(60).collect();
                    println!("  Block {}: {:?} - \"{}...\"", p.id.0, o.kind, preview);
                }
            }
        }
    }

    println!("\n=== Opaque Inlines in AFTER document ===");
    for block in &imports.import_after.canonical.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline {
                    let inlines = p.all_inlines_owned();
                    let text = extract_inline_text(&inlines);
                    let preview: String = text.chars().take(60).collect();
                    println!("  Block {}: {:?} - \"{}...\"", p.id.0, o.kind, preview);
                }
            }
        }
    }

    // Use cached diff for INSERTED blocks with opaque inlines
    let (_, diff) = &*CACHED_DIFF;

    println!("\n=== INSERTED blocks with Opaque Inlines ===");
    for (i, change) in diff.changes.iter().enumerate() {
        if let DiffChange::BlockInserted {
            block: BlockNode::Paragraph(p),
            ..
        } = change
        {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline {
                    let inlines = p.all_inlines_owned();
                    let text = extract_inline_text(&inlines);
                    let preview: String = text.chars().take(80).collect();
                    println!("  Change #{}: Block {}: {:?}", i, p.id.0, o.kind);
                    println!("    Text: \"{preview}...\"");
                }
            }
        }
    }
}

/// Tests that paragraphs with auto-numbering get synthesized number prefixes.
/// The Canada SAFE document uses Word auto-numbering for sections like:
/// "1. Events", "(a) Equity Financing", etc.
#[test]
fn numbering_synthesis_works() {
    let imports = &*CACHED_IMPORTS;

    // Find paragraphs that have numbering info
    let numbered_paragraphs: Vec<_> = imports
        .import_after
        .canonical
        .blocks
        .iter()
        .filter_map(|b| match &b.block {
            BlockNode::Paragraph(p) if p.numbering.is_some() => Some(p),
            _ => None,
        })
        .collect();

    // Should have found auto-numbered paragraphs (Canada doc has many)
    assert!(
        !numbered_paragraphs.is_empty(),
        "should find auto-numbered paragraphs in Canada SAFE"
    );

    // Check that rendered_text is set for numeric numbering
    let numeric_paragraphs: Vec<_> = numbered_paragraphs
        .iter()
        .filter(|p| p.rendered_text.is_some())
        .collect();

    assert!(
        !numeric_paragraphs.is_empty(),
        "should have paragraphs with rendered_text (synthesized numbers)"
    );

    // Find the "Events" paragraph - should have "1.\t" prefix
    let events_para = numbered_paragraphs.iter().find(|p| {
        let inlines = p.all_inlines_owned();
        let text = extract_inline_text(&inlines);
        text.starts_with("Events")
    });

    if let Some(para) = events_para {
        assert!(
            para.rendered_text.is_some(),
            "'Events' paragraph should have rendered_text"
        );
        let rendered = para.rendered_text.as_ref().unwrap();
        assert!(
            rendered.starts_with("1."),
            "rendered_text for 'Events' should start with '1.', got: {rendered:?}"
        );
    }

    // Find an "(a)" level paragraph
    let equity_financing_para = numbered_paragraphs.iter().find(|p| {
        let inlines = p.all_inlines_owned();
        let text = extract_inline_text(&inlines);
        text.starts_with("Equity Financing")
    });

    if let Some(para) = equity_financing_para {
        assert!(
            para.rendered_text.is_some(),
            "'Equity Financing' paragraph should have rendered_text"
        );
        let rendered = para.rendered_text.as_ref().unwrap();
        assert!(
            rendered.starts_with("(a)"),
            "rendered_text for 'Equity Financing' should start with '(a)', got: {rendered:?}"
        );
    }

    println!("\n=== Numbering Synthesis Results ===");
    println!("Total numbered paragraphs: {}", numbered_paragraphs.len());
    println!(
        "Paragraphs with rendered_text: {}",
        numeric_paragraphs.len()
    );
}

#[test]
fn canada_after_import_preserves_default_footer_page_field_shell() {
    let imports = &*CACHED_IMPORTS;
    let paragraph =
        footer_page_number_paragraph(&imports.import_after.canonical, "footer2.xml", "2");

    let mut has_begin = false;
    let mut has_instruction = false;
    let mut has_separate = false;
    let mut has_end = false;
    let mut saw_result = false;

    for segment in &paragraph.segments {
        for inline in &segment.inlines {
            match inline {
                InlineNode::OpaqueInline(opaque) => {
                    if let OpaqueKind::Field(data) = &opaque.kind {
                        match data.field_kind {
                            FieldKind::Begin => has_begin = true,
                            FieldKind::Instruction => {
                                has_instruction = true;
                                assert_eq!(
                                    opaque.wrapper_style_props.char_style_id.as_deref(),
                                    Some("PageNumber")
                                );
                                assert_eq!(opaque.wrapper_style_props.font_size, Some(22));
                            }
                            FieldKind::Separate => has_separate = true,
                            FieldKind::End => has_end = true,
                            FieldKind::Simple => {}
                            // This PAGE-field fixture contains no unknown-type fldChar.
                            FieldKind::Unknown(_) => {}
                        }
                    }
                }
                InlineNode::Text(text) if text.text == "2" => {
                    saw_result = true;
                    assert_eq!(
                        text.style_props.char_style_id.as_deref(),
                        Some("PageNumber")
                    );
                    assert_eq!(text.style_props.font_size, Some(22));
                }
                _ => {}
            }
        }
    }

    assert!(has_begin, "footer2 PAGE field should keep begin");
    assert!(
        has_instruction,
        "footer2 PAGE field should keep instruction"
    );
    assert!(has_separate, "footer2 PAGE field should keep separate");
    assert!(saw_result, "footer2 PAGE field should keep cached result");
    assert!(has_end, "footer2 PAGE field should keep end");
}

// NOTE: the `source_change_id_atoms_match_full_doc_segments` invariant (the
// "UNLESS PERMITTED..." repro) tests the app-layer changelet/source_change_id
// projection, which is not part of the stemma engine. It now lives with the
// consuming application's source_change_id invariant tests.
