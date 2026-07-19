//! Untouched-block fidelity gate — the redlining collateral-damage detector.
//!
//! INVARIANT (net-new; complements #12 and element_fidelity.rs): a single
//! tracked CONTENT edit leaves every block it did not touch render- and
//! content-identical. This is the property a receiving party checks when they
//! diff a returned redline: the one clause changed, and nothing else.
//!
//! Neither existing gate pins this:
//!   - `roundtrip_fidelity.rs` (#12) compares IR-to-IR — blind to import loss
//!     AND to churn localization (it can't say WHICH block changed).
//!   - `element_fidelity.rs` censuses the whole document under a styles-only
//!     edit — losses only, doc-wide, no per-block attribution, gains ignored.
//!
//! Method, per testdata fixture:
//!   1. import → insert ONE tracked sentinel paragraph after the first block
//!      (a real content edit through the edit path; un-edited export returns
//!      the original bytes and proves nothing).
//!   2. serialize → align ORIGINAL body blocks 1:1 with OUTPUT body blocks
//!      (the sentinel is removed from the output side; counts must then match).
//!   3. per untouched block, compare:
//!      a. visible text (w:t + w:delText concatenation) — EXACT, hard gate;
//!      b. element census (parent-qualified for pPr tab stops) — hard gate,
//!      GAINS AND LOSSES, except the documented KNOWN_OPEN classes;
//!      c. attribute census — hard gate under the same normalization.
//!
//! Benign-by-design normalizations (these are NOT fidelity, per the retired
//! byte-identity bar in invariants.md #2): `w:rsid*` (edit-session provenance),
//! `w:id` (annotation ids renumber per part), `xml:space` (recomputed; text is
//! compared exactly anyway), ST_OnOff literal forms ("1"/"true"/"on").
//!
//! KNOWN_OPEN classes — real, tracked, non-failing so the rest of the gate can
//! ratchet; each must either be fixed at source or promoted to a documented
//! contract:
//!   - `w:widowControl`: inherited-value materialization (same class the
//!     RunRprAuthored fix closed for run props; paragraph-level slot not yet
//!     provenance-gated).
//!
//! `w14:ligatures`, `w:bar`, and `w:between` were KNOWN_OPEN classes that
//! measured zero churn once the fidelity ratchet baseline was in place —
//! the ratchet's first ratchet-down — and were promoted to hard-gated
//! (removed from KNOWN_OPEN and dropped from the baseline).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, NodeId, RevisionInfo};
use stemma::edit::{
    BlockSpec, EditStep, EditTransaction, InsertPosition, MaterializationMode, ParagraphBlockSpec,
    parse_paragraph_markup,
};
use xmltree::{Element, XMLNode};

mod common;
use common::read_zip_entry;

const SENTINEL: &str = "UNTOUCHED-BLOCK FIDELITY SENTINEL";

/// Census keys that may differ without failing the gate. Each entry is a
/// documented open item with a known mechanism; remove entries as fixes land
/// (the ratchet). Everything NOT listed here hard-fails — which is what keeps
/// the FIXED classes fixed: w:rFonts/theme attrs, w:color, w:kern, rPr
/// character spacing (censused as `w:rPr/w:spacing`, distinct from the
/// known-open `w:pPr/w:spacing`), w:sdt, bookmarks, and fields are all absent
/// from this list on purpose.
const KNOWN_OPEN: &[&str] = &[
    // Paragraph-level inherited-value materialization: pPr slots resolved from
    // styles/docDefaults re-emitted as direct pPr (the paragraph sibling of the
    // RunRprAuthored fix; no pPr provenance gating yet).
    // ATTR-LEVEL granularity gap: element provenance flags are per-ELEMENT,
    // but import merges style/synthesized ATTRS into partially-authored
    // elements — a paragraph authoring only spacing after=240 emits the
    // resolved before+after; authored left/right ind gains the synthesized
    // tab-absorption firstLine. Fix shape: per-attr provenance or emit only
    // authored attrs of gated elements.
    "w:ind",
    "w:pPr/w:spacing",
    "w:jc",
    // w:sz residue: cs-run size materialization (cs-rtl fixtures); the
    // docDefaults-injection class is hard-gated via RunRprAuthored.font_size.
    "w:sz",
    // w:rStyle: note-reference runs synthesize their FootnoteReference/
    // EndnoteReference character style on rebuild.
    "w:rStyle",
    // Run re-segmentation (text compared exactly above, so this is pure
    // boundary churn) and the rPr envelopes that ride it.
    "w:r",
    "w:t",
    "w:rPr",
    // Table border cascade materialization (style-table borders baked onto
    // tblPr/tcPr as direct single/sz=4 borders).
    // §17.10.2 header/footer inheritance materialized: synthesized
    // headerReference/footerReference on sectPr.
    // Style/conditional-formatting materialization — shading, Heading
    // pagination props, paragraph borders baked from styles onto pPr/tcPr.
    // w:shd residue: TABLE-cell shading cascade (tcPr, banded rows) — a table
    // conditional-formatting mechanism, not paragraph pPr (which is gated).
    // w:pageBreakBefore residue: val=0 (authored-off) round-trip form.
    "w:pageBreakBefore",
    "w:wordWrap",
    "w:snapToGrid",
    "w:overflowPunct",
    "w:suppressAutoHyphens",
    "w:adjustRightInd",
    "w:pPr",
    // Numbering: style-linked numbering / literal-label conversion materialized
    // as direct numPr (includes the documented literal-"(1)"→auto-numbering
    // product flag).
    "w:numPr",
    "w:numId",
    "w:ilvl",
    "w:start",
    "w:end",
    // RESIDUALS after table-cascade provenance (bulk borders/shading/tblLook
    // materialization is hard-gated via TableFormatting/CellFormatting flags).
    // - cs/RTL font-substitution materialization: the MS-OI complex-script
    //   font-selection rules (cs font promoted onto ascii/hAnsi for RTL runs,
    //   cstheme resolution rewriting literal slots, empty <w:rFonts/> drop)
    //   rewrite authored rFonts on 3 cs spec fixtures — needs its own pass
    //   that separates render-time substitution from save-time markup
    //   (the leading/trailing-rpr prefix losses are FIXED and hard-gated via
    //   the safe-fixture blocks that carried them);
    "w:rFonts",
    // - authored-tblLook attr churn: we re-emit raw w:val PLUS all six
    //   individual booleans for clarity; a doc authoring only w:val gains
    //   attrs (spec-equal; deliberate emission style, candidate to revisit);
    "w:tblLook",
    // - border-edge residue on a handful of blocks: authored-value drift via
    //   border-conflict resolution merging table edges into authored cell
    //   sets, plus pBdr edges riding the same names;
    "w:left",
    "w:right",
    "w:top",
    "w:bottom",
    "w:shd",
    "w:tcBorders",
    // - sectPr residue: pgBorders gains explicit offsetFrom/zOrder defaults
    //   (attr-level default explicitization); pgSz/type inside a TABLE-CELL
    //   sectPr are dropped by the documented §17.6.18c strip (a cell sectPr
    //   is structurally unemittable here; fabricating a section boundary
    //   would be worse) — a documented contract, not a bug.
    "w:pgBorders",
    "w:pgSz",
    "w:type",
    // continuous-section sectPr rebuild explicit-izes spec defaults
    // (pgMar 1440/720 grid, col space=0, formProt val form) — attr-level
    // default injection, same family as pgBorders above. headerReference /
    // footerReference synthesis is HARD-GATED (fixed via StoryRef/story
    // `synthesized` provenance).
    "w:pgMar",
    "w:col",
    "w:formProt",
    "w:sectPr",
    // - width-type model gaps: tcW/tblW type flips (pct/dxa -> auto) and
    //   tblpPr attr loss (floating position not fully modeled), tcMar.
    "w:tcW",
    "w:tblW",
    "w:tblpPr",
    "w:tcMar",
    "w:gridSpan",
    "w:vMerge",
    "w:trHeight",
    // Equivalent-representation normalization: <w:cr/> re-emits as a bare
    // <w:br/> (§17.3.3.4: cr ≡ break with null type/clear) and
    // <w:noBreakHyphen/> as a literal U+2011 (documented design; Word reads
    // them identically — text comparison above applies the same reading).
    // OPEN small gap inside this class: w:br w:clear="all" is dropped
    // (HardBreakNode does not model `clear`).
    "w:cr",
    "w:br",
    "w:noBreakHyphen",
    // sectPr rebuild churn (paragraph-level section properties).
];

/// Same exclusion as element_fidelity.rs option (a): pre-existing tracked
/// changes are normalized on reserialize by design (invariant #19), so those
/// docs can't give a clean untouched-block signal.
///
/// `<w:pPrChange` is DELIBERATELY absent from this list: its previous-pPr
/// preserved remainder (unmodeled children like w:suppressLineNumbers) is now
/// captured and re-emitted verbatim, so a fixture carrying a pre-existing
/// pPrChange gives a clean untouched-block signal for its own content and is
/// no longer excluded wholesale.
const TRACKED_CHANGE_MARKERS: &[&str] = &[
    "<w:ins ",
    "<w:del ",
    "<w:rPrChange",
    "<w:tblPrChange",
    "<w:trPrChange",
    "<w:tcPrChange",
    "<w:sectPrChange",
    "<w:moveFrom",
    "<w:moveTo",
    "<w:cellIns",
    "<w:cellDel",
    "<w:cellMerge",
];

fn testdata_docx_files() -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "docx") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(std::path::Path::new("testdata"), &mut out);
    out.sort();
    out
}

fn sentinel_edit(anchor: NodeId) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor,
            position: InsertPosition::After,
            rationale: Some("untouched-block fidelity sentinel".to_string()),
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                // inserted paragraphs require an explicit role; "body" is the
                // built-in default-role alias (DEFAULT_ROLE_ALIASES)
                role: Some("body".to_string()),
                content: parse_paragraph_markup(SENTINEL).unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 900_000,
            identity: 0,
            author: Some("fidelity-gate".to_string()),
            date: Some("2026-07-02T00:00:00Z".to_string()),
            apply_op_id: None,
        },
        summary: Some("untouched-block fidelity sentinel insert".to_string()),
    }
}

/// Body-level element children of word/document.xml.
///
/// Whitespace-only text nodes MUST be preserved (`whitespace_to_characters`):
/// a separator space frequently lives alone in `<w:t xml:space="preserve"> `
/// `</w:t>`, and the default parser config drops it — which would report a
/// phantom text loss against a byte-honest output.
///
/// Body-level bookmark markers are excluded from alignment: they are
/// re-anchored to the nearest paragraph boundary by design, not blocks.
fn body_blocks(document_xml: &str) -> Vec<Element> {
    let config = xmltree::ParserConfig::new()
        .whitespace_to_characters(true)
        .cdata_to_characters(true);
    let root =
        Element::parse_with_config(document_xml.as_bytes(), config).expect("parse document.xml");
    let body = root.get_child("body").expect("w:body present").clone();
    body.children
        .into_iter()
        .filter_map(|node| match node {
            XMLNode::Element(el) => Some(el),
            _ => None,
        })
        .filter(|el| el.name != "bookmarkStart" && el.name != "bookmarkEnd")
        // w:sectPr is section properties, not a content block. Some synthesized
        // fixtures carry one mid-body (schema requires it last); the serializer
        // re-emits it at the spec position, which would shift positional
        // alignment for everything after it.
        .filter(|el| el.name != "sectPr")
        .collect()
}

fn block_text(el: &Element) -> String {
    fn collect(el: &Element, out: &mut String) {
        let is_text = el.name == "t" || el.name == "delText";
        // The rebuild path emits <w:noBreakHyphen/> as a literal U+2011 (a
        // documented design decision — Word reads them identically), so text
        // comparison must apply the same reading to the original.
        if el.name == "noBreakHyphen" {
            out.push('\u{2011}');
            return;
        }
        for child in &el.children {
            match child {
                XMLNode::Element(e) => collect(e, out),
                XMLNode::Text(t) if is_text => out.push_str(t),
                _ => {}
            }
        }
    }
    let mut out = String::new();
    collect(el, &mut out);
    out
}

fn prefixed_name(el: &Element) -> String {
    match &el.prefix {
        Some(p) => format!("{p}:{}", el.name),
        None => el.name.clone(),
    }
}

/// Does this block contain an `mc:AlternateContent` anywhere in its subtree?
///
/// A block carrying an AlternateContent is EXPECTED to change shape on the edit
/// path: MCE resolution (ISO/IEC 29500-3 §9.3) replaces the wrapper with its
/// selected branch (the Choice whose `Requires` namespaces we understand, else
/// the Fallback), so the `mc:AlternateContent`/`mc:Choice`/`mc:Fallback`
/// scaffolding and the non-selected branch drop out. This is a Word-invisible
/// normalization — Word itself resolves the same branch at render — not content
/// loss, so such blocks are excluded from the untouched-block census the same
/// way tracked blocks are. Precise (only AC-bearing blocks are skipped), so an
/// unrelated element loss elsewhere is still caught.
fn block_contains_alternate_content(el: &Element) -> bool {
    el.name == "AlternateContent"
        || el.children.iter().any(|c| match c {
            XMLNode::Element(e) => block_contains_alternate_content(e),
            _ => false,
        })
}

fn normalize_on_off(v: &str) -> &str {
    match v {
        "true" | "on" => "1",
        "false" | "off" => "0",
        other => other,
    }
}

/// Census one block: element counts (parent-qualified for tab stops) and
/// attribute name=value counts under the documented normalization.
fn census(el: &Element, parent: &str, out: &mut BTreeMap<String, i64>) {
    let name = prefixed_name(el);
    let elem_key = if name == "w:tab" && parent == "w:tabs" {
        "w:tabs/w:tab".to_string()
    } else if name == "w:spacing" {
        // Disambiguate: pPr spacing (paragraph, KNOWN_OPEN materialization
        // class) vs rPr spacing (character spacing — fixed via RunRprAuthored,
        // must stay hard-gated).
        format!("{parent}/w:spacing")
    } else {
        name.clone()
    };
    *out.entry(format!("elem {elem_key}")).or_default() += 1;

    // ST_OnOff toggles: a bare toggle element equals w:val="1" (§17.17.4) —
    // synthesize the attr so the spec-equal val="1" -> bare rewrite is a
    // non-diff while a genuine On <-> Off flip still diffs.
    const ONOFF_TOGGLES: &[&str] = &[
        "w:b",
        "w:bCs",
        "w:i",
        "w:iCs",
        "w:caps",
        "w:smallCaps",
        "w:strike",
        "w:dstrike",
        "w:vanish",
        "w:webHidden",
        "w:emboss",
        "w:imprint",
        "w:outline",
        "w:shadow",
        "w:keepNext",
        "w:keepLines",
        "w:pageBreakBefore",
        "w:widowControl",
        "w:contextualSpacing",
        "w:noProof",
        "w:snapToGrid",
        "w:rtl",
        "w:cs",
    ];
    if ONOFF_TOGGLES.contains(&name.as_str())
        && !el.attributes.iter().any(|(a, _)| a.local_name == "val")
    {
        *out.entry(format!("attr {elem_key}/@w:val=1")).or_default() += 1;
    }
    for (attr, value) in el.attributes.iter() {
        let attr_name = match &attr.prefix {
            Some(p) => format!("{p}:{}", attr.local_name),
            None => attr.local_name.clone(),
        };
        if attr_name.starts_with("w:rsid") || attr_name == "w:id" || attr_name == "xml:space" {
            continue;
        }
        let value = normalize_on_off(value);
        // Tab-stop positions ride the KNOWN_OPEN elem key; keep their attr key
        // aligned so one open item doesn't fail as two.
        let attr_key = format!("attr {elem_key}/@{attr_name}={value}");
        *out.entry(attr_key).or_default() += 1;
    }
    for child in &el.children {
        if let XMLNode::Element(e) = child {
            census(e, &name, out);
        }
    }
}

fn census_diff(orig: &Element, out: &Element) -> Vec<(String, i64, i64)> {
    let mut a = BTreeMap::new();
    let mut b = BTreeMap::new();
    census(orig, "", &mut a);
    census(out, "", &mut b);
    let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).cloned().collect();
    keys.into_iter()
        .filter_map(|k| {
            let va = a.get(&k).copied().unwrap_or(0);
            let vb = b.get(&k).copied().unwrap_or(0);
            (va != vb).then_some((k, va, vb))
        })
        .collect()
}

/// A census key naming a `w:ins` element or one of its attributes — the
/// paragraph-mark insertion marker (`w:pPr/w:rPr/w:ins`) the engine adds to the
/// anchor when a paragraph is appended at the document end (see
/// `anchor_is_final_mark`). Census keys are element-local (`elem w:ins`,
/// `attr w:ins/@…`), not paths.
fn is_paragraph_mark_ins_key(key: &str) -> bool {
    key == "elem w:ins" || key.starts_with("attr w:ins/")
}

fn is_known_open(key: &str) -> bool {
    // The theme-slot family is the LANDED RunRprAuthored fix (asciiTheme /
    // themeColor injection changes rendering per §17.3.2.26): it is never
    // known-open, whatever element carries it. Exception: the NONSTANDARD
    // camelCase `w:csTheme` (the schema attribute is lowercase `cstheme`,
    // §17.3.2.26) — legacy fixtures carry it, Word's case-sensitive reader
    // ignores it, and so does import; dropping it is consumption-faithful.
    if key.contains("w:csTheme=") {
        // Nonstandard camelCase form in legacy fixtures; case-sensitive
        // readers (incl. Word) ignore it — consumption-faithful drop.
        return true;
    }
    if key.contains("Theme") || key.contains("themeColor") {
        return false;
    }
    // Keys are "elem <name>" or "attr <name>/@<attr>=<value>"; the element
    // token decides class membership.
    let elem = key
        .strip_prefix("elem ")
        .or_else(|| key.strip_prefix("attr "))
        .unwrap_or(key);
    let elem = elem.split("/@").next().unwrap_or(elem);
    KNOWN_OPEN.contains(&elem)
}

/// Which KNOWN_OPEN class (if any) a census-diff key belongs to, for the
/// ratchet. Distinct from `is_known_open`: that function decides whether a
/// diff is allowed to pass (including the permanent csTheme normalization
/// exception and the count-drift escape valve, neither of which is a
/// KNOWN_OPEN entry); this one answers "which tracked, closable class does
/// this diff count against", by literal KNOWN_OPEN membership only. A
/// count-drift hit on a key whose element IS a KNOWN_OPEN class (e.g. `w:r`
/// re-segmentation) still counts against that class's ratchet — it's the
/// same open item, just observed via the other escape hatch.
fn known_open_class(key: &str) -> Option<&'static str> {
    let key = key.strip_prefix("(count drift) ").unwrap_or(key);
    let elem = key
        .strip_prefix("elem ")
        .or_else(|| key.strip_prefix("attr "))
        .unwrap_or(key);
    let elem = elem.split("/@").next().unwrap_or(elem);
    KNOWN_OPEN.iter().find(|&&k| k == elem).copied()
}

// ─── Ratchet mechanics ──────────────────────────────────────────────────────
//
// The baseline pins, per KNOWN_OPEN class, how many DISTINCT (fixture, block)
// pairs currently carry that class's churn. The ratchet: a class's measured
// count may never exceed its baseline. Shrinking (or zeroing) a class is
// encouraged but not auto-applied — the baseline is a committed, reviewable
// file, not a moving target — and doing it wrong (baseline entry for a class
// that left KNOWN_OPEN, or a KNOWN_OPEN class with no entry) is itself a
// hard failure, so the file can't quietly drift out of sync with the code.

const BASELINE_HEADER: &str = "\
# Untouched-block fidelity ratchet baseline.
#
# One line per KNOWN_OPEN class in untouched_block_fidelity.rs:
#   <class><TAB><distinct fixture-blocks affected>
#
# The ratchet: single_tracked_edit_leaves_untouched_blocks_identical fails if
# a class's measured count exceeds the number pinned here. Regenerate with:
#   UPDATE_FIDELITY_BASELINE=1 cargo test -p stemma --test untouched_block_fidelity
# then review and commit the diff — regeneration itself fails the test run
# (it can never silently pass CI). Every KNOWN_OPEN entry must have exactly
# one line here; a line naming a class no longer in KNOWN_OPEN is stale and
# fails too.
";

fn parse_baseline(text: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut map = BTreeMap::new();
    for (lineno, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        let class = parts.next().filter(|s| !s.is_empty()).ok_or_else(|| {
            format!(
                "baseline line {}: expected \"<class>\\t<count>\", got {raw_line:?}",
                lineno + 1
            )
        })?;
        let count_str = parts.next().ok_or_else(|| {
            format!(
                "baseline line {}: missing tab-separated count for class {class:?}",
                lineno + 1
            )
        })?;
        let count: usize = count_str.trim().parse().map_err(|e| {
            format!(
                "baseline line {}: invalid count {count_str:?} for class {class:?}: {e}",
                lineno + 1
            )
        })?;
        if map.insert(class.to_string(), count).is_some() {
            return Err(format!(
                "baseline line {}: duplicate class {class:?}",
                lineno + 1
            ));
        }
    }
    Ok(map)
}

fn render_baseline(known_open: &[&str], measured: &BTreeMap<String, usize>) -> String {
    let mut out = String::from(BASELINE_HEADER);
    let mut classes: Vec<&str> = known_open.to_vec();
    classes.sort_unstable();
    for class in classes {
        let count = measured.get(class).copied().unwrap_or(0);
        out.push_str(&format!("{class}\t{count}\n"));
    }
    out
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RatchetReport {
    /// Measured exceeds baseline: hard failure. (class, baseline, measured)
    growths: Vec<(String, usize, usize)>,
    /// Measured below baseline but nonzero: nudge only.
    shrinks: Vec<(String, usize, usize)>,
    /// Measured is zero: stronger nudge (candidate for removal from KNOWN_OPEN).
    cleans: Vec<(String, usize)>,
    /// KNOWN_OPEN classes absent from the baseline file: hard failure.
    missing_baseline: Vec<String>,
    /// Baseline entries naming a class no longer in KNOWN_OPEN: hard failure.
    stale_baseline: Vec<String>,
}

fn check_ratchet(
    known_open: &[&str],
    baseline: &BTreeMap<String, usize>,
    measured: &BTreeMap<String, usize>,
) -> RatchetReport {
    let mut report = RatchetReport::default();
    for &class in known_open {
        let Some(&base_count) = baseline.get(class) else {
            report.missing_baseline.push(class.to_string());
            continue;
        };
        let measured_count = measured.get(class).copied().unwrap_or(0);
        if measured_count > base_count {
            report
                .growths
                .push((class.to_string(), base_count, measured_count));
        } else if measured_count == 0 {
            report.cleans.push((class.to_string(), base_count));
        } else if measured_count < base_count {
            report
                .shrinks
                .push((class.to_string(), base_count, measured_count));
        }
    }
    let known_set: BTreeSet<&str> = known_open.iter().copied().collect();
    report.stale_baseline = baseline
        .keys()
        .filter(|k| !known_set.contains(k.as_str()))
        .cloned()
        .collect();
    report
}

#[test]
fn single_tracked_edit_leaves_untouched_blocks_identical() {
    let files = testdata_docx_files();
    assert!(
        files.len() > 50,
        "non-vacuity tripwire: expected testdata fixtures, found {}",
        files.len()
    );

    let mut gated = 0usize;
    let mut skipped_import = 0usize;
    let mut skipped_tracked = 0usize;
    let mut skipped_edit_refused = 0usize;
    let mut skipped_mce_blocks = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut known_open_hits: BTreeMap<String, usize> = BTreeMap::new();
    // Ratchet metric: per KNOWN_OPEN class, the count of DISTINCT (fixture,
    // block) pairs whose census diff includes that class — not the number of
    // diffing keys, so a block with e.g. both `w:ind` attrs drifting counts
    // once against the `w:ind` class, matching the "blocks affected" the
    // baseline is meant to pin.
    let mut class_hits: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let bytes = fs::read(path).expect("read fixture");
        let Some(original_xml) = read_zip_entry(&bytes, "word/document.xml") else {
            skipped_import += 1;
            continue;
        };
        if TRACKED_CHANGE_MARKERS
            .iter()
            .any(|m| original_xml.contains(m))
        {
            skipped_tracked += 1;
            continue;
        }
        let Ok(doc) = Document::parse(&bytes) else {
            skipped_import += 1;
            continue;
        };
        let anchor = {
            let canonical = &doc.snapshot().canonical;
            let Some(first) = canonical.blocks.first() else {
                skipped_import += 1;
                continue;
            };
            match &first.block {
                BlockNode::Paragraph(p) => p.id.clone(),
                BlockNode::Table(t) => t.id.clone(),
                BlockNode::OpaqueBlock(o) => o.id.clone(),
            }
        };
        // A refused edit is a typed, legitimate outcome for some fixtures
        // (e.g. anchor under structural guard); it just means this fixture
        // can't carry the sentinel — count it, don't fail.
        let edited = match doc.apply(&sentinel_edit(anchor)) {
            Ok(d) => d,
            Err(_) => {
                skipped_edit_refused += 1;
                continue;
            }
        };
        let out_bytes = edited
            .serialize(&ExportOptions::default())
            .unwrap_or_else(|e| panic!("serialize {} failed: {e:?}", path.display()));
        let out_xml =
            read_zip_entry(&out_bytes, "word/document.xml").expect("output document.xml present");

        let orig_blocks = body_blocks(&original_xml);
        let mut out_blocks = body_blocks(&out_xml);

        // Remove the sentinel paragraph (exactly one) from the output side.
        let sentinel_positions: Vec<usize> = out_blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| block_text(b).contains(SENTINEL))
            .map(|(i, _)| i)
            .collect();
        if sentinel_positions.len() != 1 {
            failures.push(format!(
                "{}: expected exactly 1 sentinel block in output, found {}",
                path.display(),
                sentinel_positions.len()
            ));
            continue;
        }
        out_blocks.remove(sentinel_positions[0]);

        if orig_blocks.len() != out_blocks.len() {
            failures.push(format!(
                "{}: block count changed {} -> {} (excluding sentinel)",
                path.display(),
                orig_blocks.len(),
                out_blocks.len()
            ));
            continue;
        }

        gated += 1;
        // When the sole body paragraph IS the sentinel's anchor (a single-block
        // body), the sentinel appends after it — i.e. at the document end — so
        // that previously-final paragraph mark now legitimately carries the
        // tracked-inserted break (`w:pPr/w:rPr/w:ins`). The document-final mark
        // can never carry a resolvable revision, so the engine attributes the
        // insertion to the preceding (here: the anchor) mark. This block is the
        // touched anchor, not an untouched block, so its added paragraph-mark
        // insertion is expected, not collateral damage.
        let anchor_is_final_mark = orig_blocks.len() == 1;
        for (idx, (orig, out)) in orig_blocks.iter().zip(out_blocks.iter()).enumerate() {
            // A block carrying an mc:AlternateContent is rewritten by MCE branch
            // resolution on the edit path (the wrapper + non-selected branch drop
            // out in favour of the selected branch). That is a documented,
            // Word-invisible normalization, not collateral damage — exclude it,
            // as tracked blocks are excluded, rather than mask element names.
            if block_contains_alternate_content(orig) {
                skipped_mce_blocks += 1;
                continue;
            }
            let orig_text = block_text(orig);
            let out_text = block_text(out);
            if orig_text != out_text {
                failures.push(format!(
                    "{} block {idx}: TEXT changed {:?} -> {:?}",
                    path.display(),
                    truncate(&orig_text),
                    truncate(&out_text)
                ));
                continue;
            }
            let mut block_classes: BTreeSet<&'static str> = BTreeSet::new();
            for (key, va, vb) in census_diff(orig, out) {
                // The anchor's newly-added paragraph-mark insertion (see
                // `anchor_is_final_mark`) is the intended attribution, not a
                // fidelity regression.
                if anchor_is_final_mark && va == 0 && is_paragraph_mark_ins_key(&key) {
                    continue;
                }
                if is_known_open(&key) {
                    if let Some(class) = known_open_class(&key) {
                        block_classes.insert(class);
                    }
                    *known_open_hits.entry(key).or_default() += 1;
                } else if va > 0 && vb > 0 {
                    // Multiplicity drift of an already-present key: run
                    // re-segmentation replicates a run's authored rPr onto the
                    // split halves (or merges them), changing counts of
                    // IDENTICAL values. Presence is what the gate protects —
                    // 0→N is injection, N→0 is loss; N→M (both >0) is
                    // segmentation churn, inventoried not failed.
                    if let Some(class) = known_open_class(&key) {
                        block_classes.insert(class);
                    }
                    *known_open_hits
                        .entry(format!("(count drift) {key}"))
                        .or_default() += 1;
                } else {
                    failures.push(format!(
                        "{} block {idx}: {key} count {va} -> {vb}",
                        path.display()
                    ));
                }
            }
            for class in block_classes {
                *class_hits.entry(class.to_string()).or_default() += 1;
            }
        }
    }

    println!(
        "untouched-block fidelity: gated={gated} skipped_tracked={skipped_tracked} \
         skipped_import={skipped_import} skipped_edit_refused={skipped_edit_refused} \
         skipped_mce_blocks={skipped_mce_blocks}"
    );
    if !known_open_hits.is_empty() {
        println!("KNOWN_OPEN churn (tracked, non-failing):");
        for (key, n) in &known_open_hits {
            println!("  {key}: {n} block(s)");
        }
    }
    assert!(
        gated >= 40,
        "non-vacuity tripwire: only {gated} fixtures were actually gated \
         (tracked={skipped_tracked} import={skipped_import} refused={skipped_edit_refused})"
    );
    assert!(
        failures.is_empty(),
        "untouched blocks changed under a single tracked edit ({} failure(s)):\n{}",
        failures.len(),
        failures.join("\n")
    );

    enforce_ratchet(&class_hits);
}

/// The baseline path is relative to the `stemma-engine` crate root, matching
/// how `testdata_docx_files` resolves `testdata/` (cargo test's cwd is the
/// crate root).
const BASELINE_PATH: &str = "tests/untouched_block_fidelity_baseline.txt";

fn enforce_ratchet(class_hits: &BTreeMap<String, usize>) {
    if std::env::var("UPDATE_FIDELITY_BASELINE").as_deref() == Ok("1") {
        let rendered = render_baseline(KNOWN_OPEN, class_hits);
        fs::write(BASELINE_PATH, &rendered).unwrap_or_else(|e| {
            panic!("UPDATE_FIDELITY_BASELINE: failed to write {BASELINE_PATH}: {e}")
        });
        panic!(
            "UPDATE_FIDELITY_BASELINE=1: baseline regenerated at {BASELINE_PATH} — review the \
             diff and commit it. This run intentionally fails so regeneration can never \
             silently pass CI."
        );
    }

    let baseline_text = fs::read_to_string(BASELINE_PATH).unwrap_or_else(|e| {
        panic!(
            "fidelity ratchet baseline missing or unreadable at {BASELINE_PATH}: {e}\n\
             Generate it with: UPDATE_FIDELITY_BASELINE=1 cargo test -p stemma --test \
             untouched_block_fidelity -- single_tracked_edit_leaves_untouched_blocks_identical\n\
             then review and commit the file. A missing baseline is never treated as \
             \"no known-open churn\" — that would silently disable the ratchet."
        )
    });
    let baseline = parse_baseline(&baseline_text)
        .unwrap_or_else(|e| panic!("malformed fidelity ratchet baseline {BASELINE_PATH}: {e}"));

    let report = check_ratchet(KNOWN_OPEN, &baseline, class_hits);

    if !report.shrinks.is_empty() {
        println!("Ratchet-down candidates (measured improved, baseline not tightened):");
        for (class, base, measured) in &report.shrinks {
            println!(
                "  {class}: {base} -> {measured} block(s) — consider lowering the baseline \
                 entry to {measured}"
            );
        }
    }
    if !report.cleans.is_empty() {
        println!("Classes with ZERO measured churn (candidates to remove from KNOWN_OPEN):");
        for (class, base) in &report.cleans {
            println!(
                "  {class}: baseline {base} -> 0 — this class appears fixed; remove it from \
                 KNOWN_OPEN in untouched_block_fidelity.rs and let it hard-gate, then delete \
                 its baseline line"
            );
        }
    }

    assert!(
        report.missing_baseline.is_empty(),
        "KNOWN_OPEN class(es) with no baseline entry in {BASELINE_PATH}: {:?}\n\
         Every KNOWN_OPEN class must have exactly one baseline line. Regenerate with \
         UPDATE_FIDELITY_BASELINE=1 (see header of {BASELINE_PATH}) and commit.",
        report.missing_baseline
    );
    assert!(
        report.stale_baseline.is_empty(),
        "baseline entry/entries in {BASELINE_PATH} name class(es) no longer in KNOWN_OPEN: \
         {:?}\nEither the class was removed from KNOWN_OPEN (delete its baseline line) or the \
         baseline is stale. Regenerate with UPDATE_FIDELITY_BASELINE=1 and commit.",
        report.stale_baseline
    );
    assert!(
        report.growths.is_empty(),
        "fidelity ratchet violated — {} KNOWN_OPEN class(es) regressed:\n{}\n\
         Either your change regressed untouched-block fidelity for these classes, or you \
         intentionally extended known-open churn — fix the code, or update the baseline in \
         the same commit with justification (UPDATE_FIDELITY_BASELINE=1, then explain why in \
         the commit message).",
        report.growths.len(),
        report
            .growths
            .iter()
            .map(|(class, base, measured)| format!(
                "  {class}: baseline {base} -> measured {measured} block(s) affected \
                 ({measured} > {base})"
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn truncate(s: &str) -> String {
    if s.len() > 80 {
        format!("{}…", &s[..s.floor_char_boundary(80)])
    } else {
        s.to_string()
    }
}

// ─── Ratchet mechanics unit tests ───────────────────────────────────────────
//
// These exercise parse_baseline/render_baseline/check_ratchet directly, in
// milliseconds, so the ratchet's own logic doesn't need the ~150s corpus run
// to verify.
#[cfg(test)]
mod ratchet_tests {
    use super::*;

    fn map(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn parse_baseline_skips_comments_and_blank_lines() {
        let text = "# header\n\n# more\nw:ind\t12\nw:jc\t0\n";
        let parsed = parse_baseline(text).expect("valid baseline");
        assert_eq!(parsed, map(&[("w:ind", 12), ("w:jc", 0)]));
    }

    #[test]
    fn parse_baseline_rejects_missing_count() {
        let err = parse_baseline("w:ind\n").unwrap_err();
        assert!(err.contains("missing tab-separated count"), "{err}");
    }

    #[test]
    fn parse_baseline_rejects_non_numeric_count() {
        let err = parse_baseline("w:ind\tmany\n").unwrap_err();
        assert!(err.contains("invalid count"), "{err}");
    }

    #[test]
    fn parse_baseline_rejects_duplicate_class() {
        let err = parse_baseline("w:ind\t1\nw:ind\t2\n").unwrap_err();
        assert!(err.contains("duplicate class"), "{err}");
    }

    #[test]
    fn render_baseline_is_sorted_and_zero_fills_absent_classes() {
        let known_open = &["w:jc", "w:ind"];
        let measured = map(&[("w:ind", 3)]);
        let rendered = render_baseline(known_open, &measured);
        let data_lines: Vec<&str> = rendered
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(data_lines, vec!["w:ind\t3", "w:jc\t0"]);
    }

    #[test]
    fn render_baseline_round_trips_through_parse_baseline() {
        let known_open = &["w:ind", "w:jc", "w:r"];
        let measured = map(&[("w:ind", 5), ("w:r", 0)]);
        let rendered = render_baseline(known_open, &measured);
        let parsed = parse_baseline(&rendered).expect("rendered baseline parses");
        assert_eq!(parsed, map(&[("w:ind", 5), ("w:jc", 0), ("w:r", 0)]));
    }

    #[test]
    fn check_ratchet_flags_growth() {
        let known_open = &["w:ind"];
        let baseline = map(&[("w:ind", 5)]);
        let measured = map(&[("w:ind", 6)]);
        let report = check_ratchet(known_open, &baseline, &measured);
        assert_eq!(report.growths, vec![("w:ind".to_string(), 5, 6)]);
        assert!(report.shrinks.is_empty());
        assert!(report.cleans.is_empty());
    }

    #[test]
    fn check_ratchet_flags_shrink_without_failing() {
        let known_open = &["w:ind"];
        let baseline = map(&[("w:ind", 5)]);
        let measured = map(&[("w:ind", 2)]);
        let report = check_ratchet(known_open, &baseline, &measured);
        assert!(report.growths.is_empty());
        assert_eq!(report.shrinks, vec![("w:ind".to_string(), 5, 2)]);
    }

    #[test]
    fn check_ratchet_flags_clean_when_measured_zero() {
        let known_open = &["w:ind"];
        let baseline = map(&[("w:ind", 5)]);
        let measured: BTreeMap<String, usize> = BTreeMap::new();
        let report = check_ratchet(known_open, &baseline, &measured);
        assert!(report.growths.is_empty());
        assert!(report.shrinks.is_empty());
        assert_eq!(report.cleans, vec![("w:ind".to_string(), 5)]);
    }

    #[test]
    fn check_ratchet_flags_missing_baseline_entry() {
        let known_open = &["w:ind", "w:jc"];
        let baseline = map(&[("w:ind", 5)]);
        let measured = map(&[("w:ind", 5)]);
        let report = check_ratchet(known_open, &baseline, &measured);
        assert_eq!(report.missing_baseline, vec!["w:jc".to_string()]);
    }

    #[test]
    fn check_ratchet_flags_stale_baseline_entry() {
        let known_open = &["w:ind"];
        let baseline = map(&[("w:ind", 5), ("w:removed-class", 3)]);
        let measured = map(&[("w:ind", 5)]);
        let report = check_ratchet(known_open, &baseline, &measured);
        assert_eq!(report.stale_baseline, vec!["w:removed-class".to_string()]);
    }

    #[test]
    fn known_open_class_maps_plain_and_count_drift_keys() {
        assert_eq!(known_open_class("elem w:ind"), Some("w:ind"));
        assert_eq!(known_open_class("attr w:ind/@w:left=100"), Some("w:ind"));
        assert_eq!(known_open_class("(count drift) elem w:r"), Some("w:r"));
        assert_eq!(known_open_class("elem w:sdt"), None);
    }

    #[test]
    fn every_known_open_class_is_covered_by_the_committed_baseline() {
        let baseline_text = fs::read_to_string(BASELINE_PATH)
            .expect("committed baseline file must exist for this test to be meaningful");
        let baseline = parse_baseline(&baseline_text).expect("committed baseline must parse");
        let report = check_ratchet(KNOWN_OPEN, &baseline, &BTreeMap::new());
        assert!(
            report.missing_baseline.is_empty(),
            "committed baseline is missing entries for: {:?}",
            report.missing_baseline
        );
        assert!(
            report.stale_baseline.is_empty(),
            "committed baseline has stale entries for: {:?}",
            report.stale_baseline
        );
    }
}
