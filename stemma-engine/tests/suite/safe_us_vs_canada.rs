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
use std::io::{Cursor, Read};
use std::sync::LazyLock;

use stemma::{
    BlockNode, DiffChange, DocxRuntime, ExportMode, FieldKind, InlineChange, InlineNode,
    OpaqueKind, RevisionInfo, SimpleRuntime, TransactionMeta, accept_all, diff_documents,
    merge_diff,
};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

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

fn extract_document_xml(docx_bytes: &[u8]) -> String {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read document.xml");
    out
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

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn is_w_tag(element: &Element, local: &str) -> bool {
    let name_local = match element.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => &element.name,
    };
    if name_local != local {
        return false;
    }
    if element.prefix.as_deref() == Some("w") {
        return true;
    }
    if element.namespace.as_deref() == Some(WORD_NS) {
        return true;
    }
    element.name == format!("w:{local}")
}

fn find_w_child<'a>(parent: &'a Element, tag: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, tag) => Some(el),
        _ => None,
    })
}

fn attr_value<'a>(element: &'a Element, qname: &str) -> Option<&'a str> {
    let local = qname.rsplit_once(':').map(|(_, l)| l).unwrap_or(qname);
    element
        .attributes
        .iter()
        .find_map(|(name, value)| (name.local_name == local).then_some(value.as_str()))
}

fn parse_document_xml_root(docx_bytes: &[u8]) -> Element {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read document.xml");
    Element::parse(Cursor::new(out.as_bytes())).expect("parse document.xml")
}

fn read_part_xml_from_docx(docx_bytes: &[u8], part_name: &str) -> String {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut file = zip
        .by_name(part_name)
        .unwrap_or_else(|e| panic!("{part_name}: {e}"));
    let mut out = String::new();
    file.read_to_string(&mut out)
        .unwrap_or_else(|e| panic!("read {part_name}: {e}"));
    out
}

fn generate_redline_docx() -> Vec<u8> {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("safe canada footer story identity regression".to_string()),
                timestamp_utc: Some("2026-03-27T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn import_doc(name: &str) -> (SimpleRuntime, stemma::CanonDoc) {
    let path = format!("testdata/safe-us-vs-canada/{name}.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {path}: {e:?}"));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}

fn generate_redline_story_xml(part_name: &str) -> Element {
    let xml = read_part_xml_from_docx(&generate_redline_docx(), part_name);
    Element::parse(Cursor::new(xml.as_bytes())).unwrap_or_else(|e| panic!("parse {part_name}: {e}"))
}

fn merge_redline_canonical() -> stemma::CanonDoc {
    let (_runtime_before, before) = import_doc("before");
    let (_runtime_after, after) = import_doc("after");
    let diff = diff_documents(&before, &after).expect("diff_documents");
    merge_diff(
        &before,
        &after,
        &diff,
        &RevisionInfo {
            revision_id: 1,
            author: Some("Stemma".to_string()),
            date: Some("2026-03-27T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    )
    .expect("merge_diff")
    .doc
}

fn run_text(run: &Element) -> String {
    fn collect_text(el: &Element, out: &mut String) {
        if is_w_tag(el, "t")
            && let Some(text) = el.get_text()
        {
            out.push_str(&text);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child {
                collect_text(node, out);
            }
        }
    }

    let mut text = String::new();
    collect_text(run, &mut text);
    text
}

fn collect_runs<'a>(el: &'a Element, out: &mut Vec<&'a Element>) {
    if is_w_tag(el, "r") {
        out.push(el);
    }
    for child in &el.children {
        if let XMLNode::Element(node) = child {
            collect_runs(node, out);
        }
    }
}

fn run_contains_field_code(run: &Element) -> bool {
    run.children.iter().any(|child| {
        matches!(
            child,
            XMLNode::Element(el) if is_w_tag(el, "fldChar") || is_w_tag(el, "instrText")
        )
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

fn paragraph_run_sequence(docx_bytes: &[u8], needle: &str) -> Vec<String> {
    fn collect_text(element: &Element, out: &mut String) {
        if (is_w_tag(element, "t") || is_w_tag(element, "delText"))
            && let Some(text) = element.get_text()
        {
            out.push_str(&text);
        }
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                collect_text(el, out);
            }
        }
    }

    fn run_sequence(run: &Element) -> Option<String> {
        if !is_w_tag(run, "r") {
            return None;
        }
        let mut out = String::new();
        for child in &run.children {
            match child {
                XMLNode::Element(el) if is_w_tag(el, "tab") => out.push_str("<TAB>"),
                XMLNode::Element(el) if is_w_tag(el, "t") || is_w_tag(el, "delText") => {
                    if let Some(text) = el.get_text() {
                        out.push_str(&text);
                    }
                }
                _ => {}
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn visit(element: &Element, needle: &str) -> Option<Vec<String>> {
        if is_w_tag(element, "p") {
            let mut text = String::new();
            collect_text(element, &mut text);
            if text.contains(needle) {
                let mut out = Vec::new();
                for child in &element.children {
                    match child {
                        XMLNode::Element(el) if is_w_tag(el, "r") => {
                            if let Some(seq) = run_sequence(el) {
                                out.push(seq);
                            }
                        }
                        XMLNode::Element(el)
                            if is_w_tag(el, "ins")
                                || is_w_tag(el, "del")
                                || el.name.ends_with(":moveFrom")
                                || el.name.ends_with(":moveTo") =>
                        {
                            for grandchild in &el.children {
                                if let XMLNode::Element(run) = grandchild
                                    && let Some(seq) = run_sequence(run)
                                {
                                    out.push(seq);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                return Some(out);
            }
        }

        for child in &element.children {
            if let XMLNode::Element(el) = child
                && let Some(found) = visit(el, needle)
            {
                return Some(found);
            }
        }
        None
    }

    let root = parse_document_xml_root(docx_bytes);
    visit(&root, needle).unwrap_or_else(|| panic!("paragraph containing {needle:?} not found"))
}

fn find_paragraph_containing_text<'a>(element: &'a Element, needle: &str) -> Option<&'a Element> {
    fn collect_text(element: &Element, out: &mut String) {
        if (is_w_tag(element, "t") || is_w_tag(element, "delText"))
            && let Some(text) = element.get_text()
        {
            out.push_str(&text);
        }
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                collect_text(el, out);
            }
        }
    }

    if is_w_tag(element, "p") {
        let mut text = String::new();
        collect_text(element, &mut text);
        if text.contains(needle) {
            return Some(element);
        }
    }

    for child in &element.children {
        if let XMLNode::Element(el) = child
            && let Some(found) = find_paragraph_containing_text(el, needle)
        {
            return Some(found);
        }
    }
    None
}

fn tracked_descendant_text(container: &Element, track_tag: &str) -> String {
    fn visit(element: &Element, track_tag: &str, inside_track: bool, out: &mut String) {
        let next_inside = inside_track || is_w_tag(element, track_tag);
        if next_inside
            && (is_w_tag(element, "t") || is_w_tag(element, "delText"))
            && let Some(text) = element.get_text()
        {
            out.push_str(&text);
        }
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                visit(el, track_tag, next_inside, out);
            }
        }
    }

    let mut out = String::new();
    visit(container, track_tag, false, &mut out);
    out
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
static CACHED_DIFF: LazyLock<(DiffSummary, stemma::DocumentDiff)> = LazyLock::new(|| {
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

/// Tests redline generation (may encounter barriers for complex docs).
#[test]
fn redline_generation() {
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

    let redline_result =
        runtime.diff_and_redline(&import_before.doc_handle, &import_after.doc_handle, meta);

    match redline_result {
        Ok(_) => {
            let output_bytes = runtime
                .export_docx(&import_before.doc_handle, ExportMode::Redline)
                .expect("export should succeed");

            let document_xml = extract_document_xml(&output_bytes);
            let has_tracked_changes =
                document_xml.contains("<w:ins") || document_xml.contains("<w:del");

            assert!(
                has_tracked_changes,
                "redline should contain tracked changes markup"
            );
        }
        Err(e) => {
            // Only UnsupportedEdit errors are acceptable (barriers, complex edits)
            // Other errors indicate bugs in the implementation
            match e.code {
                stemma::ErrorCode::UnsupportedEdit => {
                    println!(
                        "Note: redline encountered barriers (expected): {:?}",
                        e.message
                    );
                }
                _ => {
                    panic!(
                        "Unexpected error during redline generation: {:?} - {}",
                        e.code, e.message
                    );
                }
            }
        }
    }
}

#[test]
fn redline_preserves_clause_prefix_tab_run_grouping() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    let meta = TransactionMeta {
        author: "safe_us_vs_canada".to_string(),
        reason: Some("SAFE US vs Canada clause prefix regression".to_string()),
        timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
    };

    runtime
        .diff_and_redline(&import_before.doc_handle, &import_after.doc_handle, meta)
        .expect("diff_and_redline should succeed");

    let redline_bytes = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export should succeed");

    let needle = "The execution, delivery and performance";
    let target_runs = paragraph_run_sequence(&after_bytes, needle);
    let redline_runs = paragraph_run_sequence(&redline_bytes, needle);

    assert_eq!(
        target_runs[0], "<TAB>(b)",
        "fixture expectation changed: target first run should contain leading tab + prefix"
    );
    assert_eq!(
        target_runs[1], "<TAB>The execution, ",
        "fixture expectation changed: target second run should contain consumed tab + first body text"
    );
    assert_eq!(
        redline_runs[0], target_runs[0],
        "redline should keep the leading tab attached to the literal prefix run.\nredline={redline_runs:?}\ntarget={target_runs:?}"
    );
    assert!(
        redline_runs[1].starts_with(&target_runs[1]),
        "redline should attach the consumed prefix separator tab to the first body run.\nredline={redline_runs:?}\ntarget={target_runs:?}"
    );
}

#[test]
fn redline_preserves_deleted_literal_prefix_when_target_switches_to_structural_numbering() {
    let root = parse_document_xml_root(&generate_redline_docx());

    for (needle, expected_deleted_prefix) in [("Events", "1."), ("Equity Financing", "(a)")] {
        let paragraph = find_paragraph_containing_text(&root, needle)
            .unwrap_or_else(|| panic!("paragraph containing {needle:?} not found"));
        let ppr = find_w_child(paragraph, "pPr").expect("paragraph pPr");
        assert!(
            find_w_child(ppr, "numPr").is_some(),
            "{needle} paragraph should keep target structural numbering in redline"
        );

        let deleted_text = tracked_descendant_text(paragraph, "del");
        assert!(
            deleted_text.contains(expected_deleted_prefix),
            "{needle} paragraph should expose the baked base prefix as deleted text so Word reject can restore it; deleted_text={deleted_text:?}"
        );
    }
}

#[test]
fn redline_preserves_clause_auto_spacing_flags() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada autoSpace regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let redline_bytes = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export should succeed");
    let root = parse_document_xml_root(&redline_bytes);

    fn collect_text(element: &Element, out: &mut String) {
        if (is_w_tag(element, "t") || is_w_tag(element, "delText"))
            && let Some(text) = element.get_text()
        {
            out.push_str(&text);
        }
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                collect_text(el, out);
            }
        }
    }

    fn find_w_child<'a>(parent: &'a Element, tag: &str) -> Option<&'a Element> {
        parent.children.iter().find_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, tag) => Some(el),
            _ => None,
        })
    }

    fn find_para<'a>(element: &'a Element, needle: &str) -> Option<&'a Element> {
        if is_w_tag(element, "p") {
            let mut text = String::new();
            collect_text(element, &mut text);
            if text.contains(needle) {
                return Some(element);
            }
        }
        for child in &element.children {
            if let XMLNode::Element(el) = child
                && let Some(found) = find_para(el, needle)
            {
                return Some(found);
            }
        }
        None
    }

    let paragraph = find_para(&root, "The execution, delivery and performance")
        .expect("should find Canada clause paragraph in redline");
    let ppr = find_w_child(paragraph, "pPr").expect("paragraph should have pPr");
    assert!(
        find_w_child(ppr, "autoSpaceDE").is_some(),
        "redline should preserve explicit autoSpaceDE on the Canada clause paragraph"
    );
    assert!(
        find_w_child(ppr, "autoSpaceDN").is_some(),
        "redline should preserve explicit autoSpaceDN on the Canada clause paragraph"
    );
}

#[test]
fn redline_styles_part_prefers_target_body_style() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada styles regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let redline_bytes = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export should succeed");
    let cursor = Cursor::new(redline_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut file = zip.by_name("word/styles.xml").expect("word/styles.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read styles.xml");

    assert!(
        xml.contains("w:styleId=\"Body\""),
        "redline styles.xml should retain the target Body style definition"
    );
    assert!(
        !xml.contains(
            "w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:pPr><w:spacing w:before=\"240\""
        ),
        "redline styles.xml should not keep the base Normal definition when the target redefines it"
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
fn merged_redline_model_preserves_structural_numbering_when_target_adds_list_paragraph() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada numbering regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let merged = runtime
        .view(&import_before.doc_handle)
        .expect("view merged");
    let paragraph = merged
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p)
                if extract_inline_text(&p.all_inlines_owned())
                    .contains("United States federal and state income tax purposes") =>
            {
                Some(p)
            }
            _ => None,
        })
        .expect("should find merged tax paragraph");

    assert!(
        paragraph.numbering.is_some(),
        "merged redline model should preserve structural numbering for the inserted Canada list paragraph"
    );
    assert!(
        paragraph.literal_prefix.is_none(),
        "merged redline model should not degrade structural numbering into literal_prefix"
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
fn redline_preserves_target_section_footer_references() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada footer refs regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let redline_bytes = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export should succeed");
    let root = parse_document_xml_root(&redline_bytes);
    let cursor = Cursor::new(redline_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut rels_file = zip
        .by_name("word/_rels/document.xml.rels")
        .expect("word document rels");
    let mut rels_xml = String::new();
    rels_file
        .read_to_string(&mut rels_xml)
        .expect("read document rels");
    drop(rels_file);
    let rels_root = Element::parse(Cursor::new(rels_xml.as_bytes())).expect("parse document rels");

    fn find_w_child<'a>(parent: &'a Element, tag: &str) -> Option<&'a Element> {
        parent.children.iter().find_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, tag) => Some(el),
            _ => None,
        })
    }

    let body = find_w_child(&root, "body").expect("document body");
    let sect_pr = find_w_child(body, "sectPr").expect("body sectPr");
    let footer_refs: Vec<(String, String)> = sect_pr
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "footerReference") => {
                let kind = el
                    .attributes
                    .iter()
                    .find_map(|(name, value)| {
                        let rendered = name.to_string();
                        (rendered == "w:type" || rendered.ends_with(":type") || rendered == "type")
                            .then_some(value.clone())
                    })
                    .unwrap_or_default();
                let rid = el
                    .attributes
                    .iter()
                    .find_map(|(name, value)| {
                        let rendered = name.to_string();
                        (rendered == "r:id" || rendered.ends_with(":id") || rendered == "id")
                            .then_some(value.clone())
                    })
                    .unwrap_or_default();
                let target = rels_root
                    .children
                    .iter()
                    .find_map(|rel_child| match rel_child {
                        XMLNode::Element(rel) if rel.name.ends_with("Relationship") => {
                            let rel_id = rel.attributes.iter().find_map(|(name, value)| {
                                (name.to_string() == "Id").then_some(value.as_str())
                            })?;
                            (rel_id == rid).then(|| {
                                rel.attributes
                                    .iter()
                                    .find_map(|(name, value)| {
                                        (name.to_string() == "Target").then_some(value.clone())
                                    })
                                    .unwrap_or_default()
                            })
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((kind, target))
            }
            _ => None,
        })
        .collect();

    assert_eq!(
        footer_refs,
        vec![
            ("default".to_string(), "footer4.xml".to_string()),
            ("first".to_string(), "footer5.xml".to_string()),
        ],
        "redline carries the target body section's AUTHORED footer references \
         — the even footer1.xml ref is §17.10.2 inheritance from an earlier \
         section (render-time), which the target's own sectPr does not author \
         and the redline must not materialize"
    );
}

#[test]
fn redline_preserves_first_section_footer_references() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada first-section footer refs regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let redline_bytes = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export should succeed");
    let cursor = Cursor::new(redline_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut rels_xml = String::new();
    {
        let mut rels_file = zip
            .by_name("word/_rels/document.xml.rels")
            .expect("word document rels");
        rels_file
            .read_to_string(&mut rels_xml)
            .expect("read document rels");
    }
    let rels_root = Element::parse(Cursor::new(rels_xml.as_bytes())).expect("parse document rels");
    let mut doc_xml = String::new();
    {
        let mut doc_file = zip.by_name("word/document.xml").expect("word/document.xml");
        doc_file
            .read_to_string(&mut doc_xml)
            .expect("read document.xml");
    }
    let doc_root = Element::parse(Cursor::new(doc_xml.as_bytes())).expect("parse document.xml");

    fn footer_refs_for_sect(sect_pr: &Element, rels_root: &Element) -> Vec<(String, String)> {
        sect_pr
            .children
            .iter()
            .filter_map(|child| match child {
                XMLNode::Element(el) if is_w_tag(el, "footerReference") => {
                    let kind = el
                        .attributes
                        .iter()
                        .find_map(|(name, value)| {
                            let rendered = name.to_string();
                            (rendered == "w:type"
                                || rendered.ends_with(":type")
                                || rendered == "type")
                                .then_some(value.clone())
                        })
                        .unwrap_or_default();
                    let rid = el
                        .attributes
                        .iter()
                        .find_map(|(name, value)| {
                            let rendered = name.to_string();
                            (rendered == "r:id" || rendered.ends_with(":id") || rendered == "id")
                                .then_some(value.clone())
                        })
                        .unwrap_or_default();
                    let target = rels_root
                        .children
                        .iter()
                        .find_map(|rel_child| match rel_child {
                            XMLNode::Element(rel) if rel.name.ends_with("Relationship") => {
                                let rel_id = rel.attributes.iter().find_map(|(name, value)| {
                                    (name.to_string() == "Id").then_some(value.as_str())
                                })?;
                                (rel_id == rid).then(|| {
                                    rel.attributes
                                        .iter()
                                        .find_map(|(name, value)| {
                                            (name.to_string() == "Target").then_some(value.clone())
                                        })
                                        .unwrap_or_default()
                                })
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    Some((kind, target))
                }
                _ => None,
            })
            .collect()
    }

    fn collect_sect_prs<'a>(element: &'a Element, out: &mut Vec<&'a Element>) {
        if is_w_tag(element, "sectPr") {
            out.push(element);
        }
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                collect_sect_prs(el, out);
            }
        }
    }

    let mut sects = Vec::new();
    collect_sect_prs(&doc_root, &mut sects);

    let first_section_refs = footer_refs_for_sect(sects.first().expect("first sectPr"), &rels_root);
    assert_eq!(
        first_section_refs,
        vec![
            ("even".to_string(), "footer1.xml".to_string()),
            ("default".to_string(), "footer2.xml".to_string()),
            ("first".to_string(), "footer3.xml".to_string()),
        ],
        "redline should preserve the target first section footer refs"
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
fn merged_redline_model_preserves_first_section_footer_refs_on_diff_block_id() {
    let (_, diff) = &*CACHED_DIFF;
    let section_block_id = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified {
                block_id,
                new_block,
                ..
            } => match new_block {
                BlockNode::Paragraph(p)
                    if footer_refs_from_section_properties(p).as_ref()
                        == Some(&vec![
                            ("even".to_string(), "footer1.xml".to_string()),
                            ("default".to_string(), "footer2.xml".to_string()),
                            ("first".to_string(), "footer3.xml".to_string()),
                        ]) =>
                {
                    Some(block_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("diff should contain the first section-break paragraph as a BlockModified");

    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada merged first-section block id".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let merged = runtime
        .view(&import_before.doc_handle)
        .expect("view merged");
    let footer_refs = merged
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) if p.id == section_block_id => {
                footer_refs_from_section_properties(p)
            }
            _ => None,
        })
        .expect("merged doc should retain the diff block id for the section break");

    assert_eq!(
        footer_refs,
        vec![
            ("even".to_string(), "footer1.xml".to_string()),
            ("default".to_string(), "footer2.xml".to_string()),
            ("first".to_string(), "footer3.xml".to_string()),
        ],
        "merged block with the diff section-break id should preserve the target first section footer refs"
    );
}

#[test]
fn apply_result_preserves_first_section_footer_refs_on_diff_block_id() {
    let (_, diff) = &*CACHED_DIFF;
    let section_block_id = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified {
                block_id,
                new_block,
                ..
            } => match new_block {
                BlockNode::Paragraph(p)
                    if footer_refs_from_section_properties(p).as_ref()
                        == Some(&vec![
                            ("even".to_string(), "footer1.xml".to_string()),
                            ("default".to_string(), "footer2.xml".to_string()),
                            ("first".to_string(), "footer3.xml".to_string()),
                        ]) =>
                {
                    Some(block_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("diff should contain the first section-break paragraph as a BlockModified");

    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    let apply = runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada apply-result first-section block id".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let footer_refs = apply
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) if p.id == section_block_id => {
                footer_refs_from_section_properties(p)
            }
            _ => None,
        })
        .expect("apply result should retain the diff block id for the section break");

    assert_eq!(
        footer_refs,
        vec![
            ("even".to_string(), "footer1.xml".to_string()),
            ("default".to_string(), "footer2.xml".to_string()),
            ("first".to_string(), "footer3.xml".to_string()),
        ],
        "apply result canonical should preserve the target first section footer refs"
    );
}

#[test]
fn merged_redline_model_preserves_first_section_footer_refs() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada merged first-section footer refs".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let merged = runtime
        .view(&import_before.doc_handle)
        .expect("view merged");
    let footer_refs = merged
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) => footer_refs_from_section_properties(p),
            _ => None,
        })
        .expect("merged doc should preserve a first section-break paragraph");

    assert_eq!(
        footer_refs,
        vec![
            ("even".to_string(), "footer1.xml".to_string()),
            ("default".to_string(), "footer2.xml".to_string()),
            ("first".to_string(), "footer3.xml".to_string()),
        ],
        "merged model should preserve the target first section footer refs before serialization"
    );
}

#[test]
fn merged_redline_model_preserves_target_section_footer_refs() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada body sectPr regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let merged = runtime
        .view(&import_before.doc_handle)
        .expect("view merged");
    let footer_refs: Vec<(String, String)> = merged
        .canonical
        .body_section_properties
        .as_ref()
        .expect("merged doc should keep body section properties")
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
        "merged model should preserve the target section footer refs before serialization"
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

#[test]
fn merged_redline_model_preserves_clause_body_run_boundaries() {
    let before_bytes =
        fs::read("testdata/safe-us-vs-canada/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-canada/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "safe_us_vs_canada".to_string(),
                reason: Some("SAFE US vs Canada merged-model regression".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            },
        )
        .expect("diff_and_redline should succeed");

    let merged = runtime
        .view(&import_before.doc_handle)
        .expect("view merged");
    let paragraph = merged
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
        .expect("should find merged Canada clause paragraph");

    let visible_text_runs: Vec<String> = paragraph
        .segments
        .iter()
        .filter(|seg| !matches!(seg.status, stemma::TrackingStatus::Deleted(_)))
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(
        visible_text_runs.first().map(String::as_str),
        Some("The execution, "),
        "merged redline model should preserve the first target body run boundary after the stripped clause prefix"
    );
    assert_eq!(
        visible_text_runs.get(1).map(String::as_str),
        Some("delivery"),
        "merged redline model should preserve the second target body run boundary in the Canada clause paragraph"
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

#[test]
fn canada_redline_preserves_default_footer_page_field_shell_and_formatting() {
    let root = generate_redline_story_xml("word/footer2.xml");

    let mut runs = Vec::new();
    collect_runs(&root, &mut runs);

    let field_runs: Vec<&Element> = runs
        .iter()
        .copied()
        .filter(|run| run_contains_field_code(run))
        .collect();
    assert!(
        !field_runs.is_empty(),
        "redline footer2 should contain PAGE field runs"
    );

    let mut saw_begin = false;
    let mut saw_instruction = false;
    let mut saw_separate = false;
    let mut saw_end = false;

    for run in field_runs {
        let rpr = find_w_child(run, "rPr").expect("field run should keep rPr");
        let rstyle = find_w_child(rpr, "rStyle").expect("field run should keep PageNumber style");
        let sz = find_w_child(rpr, "sz").expect("field run should keep explicit 11pt size");
        assert_eq!(attr_value(rstyle, "w:val"), Some("PageNumber"));
        assert_eq!(attr_value(sz, "w:val"), Some("22"));

        if let Some(fld_char) = find_w_child(run, "fldChar") {
            match attr_value(fld_char, "w:fldCharType") {
                Some("begin") => saw_begin = true,
                Some("separate") => saw_separate = true,
                Some("end") => saw_end = true,
                _ => {}
            }
        }
        if let Some(instr) = find_w_child(run, "instrText") {
            saw_instruction = true;
            let instr_text = instr
                .get_text()
                .map(|text| text.to_string())
                .unwrap_or_default();
            assert_eq!(instr_text.trim(), "PAGE");
        }
    }

    fn find_run_with_text<'a>(el: &'a Element, needle: &str) -> Option<&'a Element> {
        if is_w_tag(el, "r") && run_text(el) == needle {
            return Some(el);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child
                && let Some(found) = find_run_with_text(node, needle)
            {
                return Some(found);
            }
        }
        None
    }

    let result_run =
        find_run_with_text(&root, "2").expect("redline footer2 should contain PAGE result run");
    let result_rpr = find_w_child(result_run, "rPr").expect("result run should keep rPr");
    let result_rstyle =
        find_w_child(result_rpr, "rStyle").expect("result run should keep PageNumber style");
    let result_sz =
        find_w_child(result_rpr, "sz").expect("result run should keep explicit 11pt size");
    assert_eq!(attr_value(result_rstyle, "w:val"), Some("PageNumber"));
    assert_eq!(attr_value(result_sz, "w:val"), Some("22"));

    assert!(saw_begin, "redline footer2 should keep PAGE begin");
    assert!(
        saw_instruction,
        "redline footer2 should keep PAGE instruction"
    );
    assert!(saw_separate, "redline footer2 should keep PAGE separate");
    assert!(saw_end, "redline footer2 should keep PAGE end");
}

#[test]
fn canada_redline_preserves_target_default_footer_paragraph_count() {
    let root = generate_redline_story_xml("word/footer2.xml");
    let paragraph_count = root
        .children
        .iter()
        .filter(|child| matches!(child, XMLNode::Element(el) if is_w_tag(el, "p")))
        .count();
    assert_eq!(
        paragraph_count, 2,
        "redline footer2.xml should keep the target's two-paragraph structure"
    );
}

#[test]
fn canada_merged_canonical_preserves_default_footer_page_field_shell() {
    let doc = merge_redline_canonical();
    let paragraph = footer_page_number_paragraph(&doc, "footer2.xml", "2");

    let mut has_begin = false;
    let mut has_instruction = false;
    let mut has_separate = false;
    let mut has_end = false;
    let mut saw_result = false;

    for segment in &paragraph.segments {
        for inline in &segment.inlines {
            match (&segment.status, inline) {
                (_, InlineNode::OpaqueInline(opaque)) => {
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
                (_, InlineNode::Text(text)) if text.text == "2" => {
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

    assert!(has_begin, "merged footer2 should keep PAGE begin");
    assert!(
        has_instruction,
        "merged footer2 should keep PAGE instruction"
    );
    assert!(has_separate, "merged footer2 should keep PAGE separate");
    assert!(saw_result, "merged footer2 should keep PAGE cached result");
    assert!(has_end, "merged footer2 should keep PAGE end");
}

// =============================================================================
// Fixpoint invariant
// =============================================================================

/// Fixpoint invariant: `diff(accept_all(merge_diff(A, B, diff(A, B))), B) == empty`
///
/// Verifies that diff → merge → accept_all faithfully transforms A into B
/// in canonical space. This is a daily test (no #[ignore]) because the
/// safe-us-vs-canada fixture previously failed due to OpaqueBlocks in the
/// target being invisible to the diff, causing a block count mismatch in
/// fix_numbering_drift_for_normal_blocks → formatting sync skipped → residuals.
///
#[test]
fn fixpoint_diff_merge_accept_rediff_is_empty() {
    let imports = &*CACHED_IMPORTS;
    let canon_a = &imports.import_before.canonical;
    let canon_b = &imports.import_after.canonical;

    let revision = RevisionInfo {
        revision_id: 1,
        author: Some("fixpoint-test".to_string()),
        date: Some("2025-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    };

    // Step 1: diff A → B
    let diff = diff_documents(canon_a, canon_b).expect("diff should succeed");

    // Step 2: merge diff into A with tracked changes
    let mut merged = merge_diff(canon_a, canon_b, &diff, &revision).expect("merge should succeed");

    // Step 3: accept all tracked changes
    accept_all(&mut merged.doc);

    // Step 4: re-diff — should be empty (fixpoint)
    let fixpoint_diff = diff_documents(&merged.doc, canon_b).expect("fixpoint diff should succeed");

    if !fixpoint_diff.changes.is_empty() {
        let descriptions: Vec<String> = fixpoint_diff
            .changes
            .iter()
            .take(10)
            .map(|c| match c {
                DiffChange::BlockDeleted { old_text, .. } => {
                    format!(
                        "BlockDeleted: {:?}",
                        old_text.chars().take(80).collect::<String>()
                    )
                }
                DiffChange::BlockInserted { block, .. } => {
                    format!(
                        "BlockInserted: {:?}",
                        format!("{block:?}").chars().take(80).collect::<String>()
                    )
                }
                DiffChange::BlockModified {
                    old_text, new_text, ..
                } => {
                    if old_text == new_text {
                        format!(
                            "BlockModified (formatting-only): text={:?}",
                            old_text.chars().take(60).collect::<String>()
                        )
                    } else {
                        format!(
                            "BlockModified: old={:?} new={:?}",
                            old_text.chars().take(60).collect::<String>(),
                            new_text.chars().take(60).collect::<String>()
                        )
                    }
                }
                other => format!("{other:?}").chars().take(120).collect(),
            })
            .collect();

        panic!(
            "fixpoint invariant violated: accept_all(merge_diff(A, B, diff(A, B))) differs \
             from B with {} residual change(s):\n    {}",
            fixpoint_diff.changes.len(),
            descriptions.join("\n    ")
        );
    }
}

// NOTE: the `source_change_id_atoms_match_full_doc_segments` invariant (the
// "UNLESS PERMITTED..." repro) tests the app-layer changelet/source_change_id
// projection, which is not part of the stemma engine. It now lives with the
// consuming application's source_change_id invariant tests.
