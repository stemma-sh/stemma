//! Text replacement reaches text inside hyperlinks and content controls.
//!
//! Domain rule: a hyperlink's display text (§17.16.22 `w:hyperlink`) and an
//! inline content control's value (§17.5.2.36 `w:sdtContent`) are text of the
//! paragraph the reader sees. A find/replace — the replace-text worklist and
//! replace-all alike — replaces an occurrence there as it does in the
//! paragraph's own runs: as a tracked change inside the link or control (Word
//! keeps a tracked edit inside a link or a control inside it), or directly.
//! For every case Reject All gives the original reading, Accept All the
//! replaced reading, the direct edit equals Accept All, and all of this holds
//! after the edited document is saved and reopened.
//!
//! What still refuses, naming the object:
//! - a control whose content is locked (`w:lock` `contentLocked` or
//!   `sdtContentLocked`, §17.5.2.23); `sdtLocked` only keeps the control from
//!   being deleted, so its value is edited;
//! - an occurrence that runs across a link's or control's edge: which part of
//!   the replacement belongs inside the object is not known, so the edge is a
//!   wall like any other (refused under `fail`, reported under `skip`).
//!
//! A control bound to a data store (§17.5.2.6) shows the store's value and
//! Word refills it from the store on open, so a replacement inside it also
//! writes the store, as the single-control edit routes do.
//!
//! Observed with Word: these tracked edits open without repair, show their
//! changes inside the link or control, and Accept All / Reject All give the
//! readings asserted here; the link stays one link to its target, each run
//! keeps its formatting, an `sdtLocked` control keeps its lock, and a bound
//! control's store holds the resolved text. Word blocks typing in a
//! `contentLocked` control. Word's own replacement of a selection that starts
//! outside a link and ends inside it deletes the whole link and types the new
//! text outside any link, so no edit keeps the link and its text both: hence
//! the wall.

use std::collections::BTreeSet;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
use stemma::edit::{
    BarrierPolicy, EditError, EditStep, EditTransaction, ExpectedMatches, FindReplaceOptions,
    FindReplaceScope, InlineObjectText, MatchMode, MaterializationMode, ObjectTextBlock,
    ReplaceTextError, ReplaceTextOptions, ReplaceTextPlan, ReplaceTextScope, plan_find_replace_all,
    plan_replace_text,
};

const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Orchard Lane Lease</dc:title></cp:coreProperties>"#;

const TITLE_BINDING: &str = r#"<w:dataBinding w:prefixMappings="xmlns:ns0='http://schemas.openxmlformats.org/package/2006/metadata/core-properties' xmlns:ns1='http://purl.org/dc/elements/1.1/'" w:xpath="/ns0:coreProperties[1]/ns1:title[1]" w:storeItemID="{6C3C8BC8-F283-45AE-878A-BAB7291924A1}"/>"#;

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdL1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/orchard" TargetMode="External"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("docProps/core.xml", CORE),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn link(inner: &str) -> String {
    format!(r#"<w:hyperlink r:id="rIdL1">{inner}</w:hyperlink>"#)
}

/// An inline content control: `pr` goes inside `w:sdtPr`, `inner` is its
/// content.
fn control(pr: &str, inner: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:id w:val="41"/>{pr}</w:sdtPr><w:sdtContent>{inner}</w:sdtContent></w:sdt>"#
    )
}

fn para(inner: &str) -> String {
    format!("<w:p>{inner}</w:p>")
}

// ── Reading ─────────────────────────────────────────────────────────────

/// The `w:t` text of a resolved fragment, in order.
fn fragment_text(raw: &[u8]) -> String {
    let xml = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut rest = xml.as_ref();
    while let Some(at) = rest.find("<w:t") {
        rest = &rest[at + 4..];
        if !(rest.starts_with('>') || rest.starts_with(' ')) {
            continue;
        }
        let open = rest.find('>').unwrap();
        if rest[..open].ends_with('/') {
            rest = &rest[open + 1..];
            continue;
        }
        let close = rest.find("</w:t>").unwrap();
        out.push_str(&rest[open + 1..close]);
        rest = &rest[close..];
    }
    out
}

/// Each body paragraph's reading (cells included), a link's display text and
/// a control's value in place, one line per paragraph. Meaningful only for a
/// resolved document (no pending changes).
fn reading(doc: &Document) -> String {
    fn block(node: &BlockNode, out: &mut Vec<String>) {
        match node {
            BlockNode::Paragraph(p) => {
                let mut line = String::new();
                for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                    match inline {
                        InlineNode::Text(t) => line.push_str(&t.text),
                        InlineNode::OpaqueInline(o) => match &o.kind {
                            OpaqueKind::Hyperlink(link) => line.push_str(&link.text),
                            OpaqueKind::Sdt => {
                                line.push_str(&fragment_text(o.raw_xml.as_deref().unwrap()))
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                out.push(line);
            }
            BlockNode::Table(t) => t
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .flat_map(|c| &c.blocks)
                .for_each(|b| block(b, out)),
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let mut out = Vec::new();
    for tracked in &doc.snapshot().canonical.blocks {
        block(&tracked.block, &mut out);
    }
    out.join("\n")
}

fn reopen(doc: &Document) -> Document {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

fn part(doc: &Document, name: &str) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get(name)
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// `xml` with self-closing tags spelled without the space.
fn bare_xml(xml: &str) -> String {
    xml.replace(" />", "/>")
}

/// The `w:id` of every `w:ins` and `w:del` in the saved body.
fn wire_revision_ids(doc: &Document) -> Vec<String> {
    let xml = part(doc, "word/document.xml");
    let mut ids = Vec::new();
    for tag in ["<w:ins ", "<w:del "] {
        let mut rest = xml.as_str();
        while let Some(at) = rest.find(tag) {
            rest = &rest[at + tag.len()..];
            let element = &rest[..rest.find('>').unwrap()];
            let id = element.split("w:id=\"").nth(1).unwrap();
            ids.push(id[..id.find('"').unwrap()].to_string());
        }
    }
    ids
}

// ── Planning and applying ───────────────────────────────────────────────

fn options(old: &str, new: &str, expected: ExpectedMatches) -> ReplaceTextOptions {
    ReplaceTextOptions {
        old: old.to_string(),
        new: new.to_string(),
        author: "Reviewer".to_string(),
        scope: ReplaceTextScope::BodyAndTables,
        expected,
        match_mode: MatchMode::Exact,
        on_barrier_match: BarrierPolicy::Fail,
    }
}

fn plan(doc: &Document, options: &ReplaceTextOptions) -> ReplaceTextPlan {
    plan_replace_text(&doc.snapshot().canonical, options).expect("the replacement plans")
}

fn apply(doc: &Document, steps: &[EditStep], mode: MaterializationMode) -> Document {
    doc.apply(&EditTransaction {
        steps: steps.to_vec(),
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-11-10T21:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .unwrap_or_else(|e| panic!("apply {mode:?}: {:?}: {}", e.code, e.message))
}

/// The laws of a replacement: tracked, it creates revisions with distinct wire
/// ids; Reject All reads `before` and Accept All `after`, in memory and after
/// reopen; the direct edit reads `after` and creates no revisions. Returns the
/// tracked document.
fn assert_laws(doc: &Document, steps: &[EditStep], before: &str, after: &str) -> Document {
    assert_eq!(reading(doc), before, "the fixture reads as stated");
    let tracked = apply(doc, steps, MaterializationMode::TrackedChange);
    assert!(
        !tracked.revisions().is_empty(),
        "a tracked replacement creates revisions"
    );
    let ids = wire_revision_ids(&tracked);
    assert_eq!(
        ids.len(),
        ids.iter().collect::<BTreeSet<_>>().len(),
        "each w:ins and w:del has its own id (§17.13.1): {ids:?}"
    );
    let reopened = reopen(&tracked);
    for (label, document) in [("in memory", &tracked), ("reopened", &reopened)] {
        let rejected = document.read_rejected().expect("reject all");
        let accepted = document.read_accepted().expect("accept all");
        assert_eq!(reading(&rejected), before, "Reject All, {label}");
        assert_eq!(reading(&accepted), after, "Accept All, {label}");
        assert_eq!(
            reading(&reopen(&rejected)),
            before,
            "Reject All saved, {label}"
        );
        assert_eq!(
            reading(&reopen(&accepted)),
            after,
            "Accept All saved, {label}"
        );
    }
    let direct = apply(doc, steps, MaterializationMode::Direct);
    assert_eq!(reading(&direct), after, "the direct edit");
    assert_eq!(
        reading(&reopen(&direct)),
        after,
        "the direct edit, reopened"
    );
    assert!(
        direct.revisions().is_empty(),
        "a direct edit leaves no revisions: {:?}",
        direct.revisions()
    );
    tracked
}

/// The refusal's occurrences as (object kind, reason).
fn refused(
    result: Result<ReplaceTextPlan, ReplaceTextError>,
) -> Vec<(InlineObjectText, ObjectTextBlock)> {
    match result {
        Err(ReplaceTextError::Engine(EditError::ReplaceTextInsideInlineObjects {
            occurrences,
            ..
        })) => occurrences
            .into_iter()
            .map(|o| (o.object, o.reason))
            .collect(),
        Err(other) => panic!("expected the inline-object refusal, got {other:?}"),
        Ok(plan) => panic!("expected a refusal, planned {:?}", plan.steps),
    }
}

// ── A match wholly inside a link ────────────────────────────────────────

#[test]
fn a_match_inside_link_text_is_replaced_inside_the_link() {
    let body = para(&format!(
        "{}{}{}",
        run("Signed under "),
        link(&run("the Orchard Lane terms")),
        run(".")
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(
        &doc,
        &options("Orchard Lane", "Birch Row", ExpectedMatches::Count(1)),
    );
    assert_eq!(plan.matches.len(), 1, "the one occurrence is counted");
    let tracked = assert_laws(
        &doc,
        &plan.steps,
        "Signed under the Orchard Lane terms.",
        "Signed under the Birch Row terms.",
    );
    // The replacement stands inside the link: it is still one link, now
    // reading the new text, with its target kept.
    let accepted = tracked.read_accepted().unwrap();
    let BlockNode::Paragraph(first) = &accepted.snapshot().canonical.blocks[0].block else {
        panic!("the body starts with the paragraph");
    };
    let links: Vec<_> = first
        .segments
        .iter()
        .flat_map(|s| s.inlines.clone())
        .filter_map(|inline| match inline {
            InlineNode::OpaqueInline(o) => match o.kind {
                OpaqueKind::Hyperlink(link) => Some((link.text, link.url)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        links,
        vec![(
            "the Birch Row terms".to_string(),
            Some("https://example.com/orchard".to_string())
        )]
    );
    // Only the changed words are tracked: the kept words stay in place.
    let xml = part(&tracked, "word/document.xml");
    assert!(xml.contains("<w:delText>Orchard Lane</w:delText>"), "{xml}");
}

#[test]
fn two_occurrences_in_one_link_are_two_changes() {
    let body = para(&link(&format!(
        "{}{}",
        run("Lane notice, "),
        r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Lane schedule</w:t></w:r>"#
    )));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(&doc, &options("Lane", "Row", ExpectedMatches::All));
    assert_eq!(plan.matches.len(), 2);
    let tracked = assert_laws(
        &doc,
        &plan.steps,
        "Lane notice, Lane schedule",
        "Row notice, Row schedule",
    );
    // The second replacement keeps the bold of the text it replaces, the first
    // stays plain.
    let accepted = bare_xml(&part(
        &tracked.read_accepted().unwrap(),
        "word/document.xml",
    ));
    assert!(accepted.contains("<w:r><w:t>Row</w:t></w:r>"), "{accepted}");
    assert!(
        accepted.contains("<w:b/></w:rPr><w:t>Row</w:t>"),
        "{accepted}"
    );
}

#[test]
fn a_match_in_the_paragraph_and_one_in_its_link_are_both_replaced() {
    let body = para(&format!(
        "{}{}{}",
        run("Orchard tenant: see "),
        link(&run("Orchard terms")),
        run(" for Orchard.")
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(
        &doc,
        &options("Orchard", "Birch", ExpectedMatches::Count(3)),
    );
    assert_laws(
        &doc,
        &plan.steps,
        "Orchard tenant: see Orchard terms for Orchard.",
        "Birch tenant: see Birch terms for Birch.",
    );
}

// ── A match crossing a link's edge ──────────────────────────────────────

#[test]
fn a_match_across_a_links_edge_is_a_wall() {
    let body = para(&format!(
        "{}{}",
        run("Signed under the "),
        link(&run("Orchard Lane terms"))
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let link_id = match &doc.snapshot().canonical.blocks[0].block {
        BlockNode::Paragraph(p) => p
            .segments
            .iter()
            .flat_map(|s| &s.inlines)
            .find_map(|i| match i {
                InlineNode::OpaqueInline(o) => Some(o.id.to_string()),
                _ => None,
            })
            .unwrap(),
        _ => unreachable!(),
    };
    // fail: refused, naming the link's edge.
    match plan_replace_text(
        &doc.snapshot().canonical,
        &options("the Orchard", "a Birch", ExpectedMatches::Count(1)),
    ) {
        Err(ReplaceTextError::Engine(EditError::FindReplaceBarrierStraddle { walls, .. })) => {
            assert_eq!(walls, vec![format!("the edge of hyperlink '{link_id}'")]);
        }
        other => panic!("expected a straddle refusal, got {other:?}"),
    }
    // skip: nothing is planned, and the skip is reported.
    let mut skip = options("the Orchard", "a Birch", ExpectedMatches::All);
    skip.on_barrier_match = BarrierPolicy::Skip;
    let plan = plan(&doc, &skip);
    assert!(plan.steps.is_empty());
    assert_eq!(plan.skipped_straddles.len(), 1);
    assert_eq!(plan.skipped_straddles[0].wall, "hyperlink_edge");
}

// ── Content controls ────────────────────────────────────────────────────

#[test]
fn a_match_inside_a_plain_text_control_is_replaced_inside_it() {
    let body = para(&format!(
        "{}{}",
        run("Tenant: "),
        control("<w:text/>", &run("Orchard Lane Holdings"))
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(
        &doc,
        &options("Orchard Lane", "Birch Row", ExpectedMatches::Count(1)),
    );
    let tracked = assert_laws(
        &doc,
        &plan.steps,
        "Tenant: Orchard Lane Holdings",
        "Tenant: Birch Row Holdings",
    );
    // The tracked change stands inside the control's content.
    let xml = part(&tracked, "word/document.xml");
    let content = &xml[xml.find("<w:sdtContent>").unwrap()..xml.find("</w:sdtContent>").unwrap()];
    assert!(
        content.contains("<w:ins ") && content.contains("<w:del "),
        "{xml}"
    );
}

#[test]
fn a_match_inside_a_rich_text_control_keeps_each_runs_formatting() {
    let body = para(&control(
        "",
        &format!(
            "{}{}",
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Orchard Lane</w:t></w:r>"#,
            run(" and Orchard Lane annex")
        ),
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(&doc, &options("Orchard", "Birch", ExpectedMatches::All));
    assert_eq!(plan.matches.len(), 2);
    let tracked = assert_laws(
        &doc,
        &plan.steps,
        "Orchard Lane and Orchard Lane annex",
        "Birch Lane and Birch Lane annex",
    );
    // The first replacement is bold like the word it replaces; the second is not.
    let accepted = bare_xml(&part(
        &tracked.read_accepted().unwrap(),
        "word/document.xml",
    ));
    assert!(
        accepted.contains("<w:b/></w:rPr><w:t>Birch Lane</w:t>"),
        "{accepted}"
    );
    assert_eq!(accepted.matches("<w:b/>").count(), 1, "{accepted}");
}

#[test]
fn a_data_bound_control_writes_its_store() {
    let body = para(&format!(
        "{}{}",
        run("Re: "),
        control(
            &format!("{TITLE_BINDING}<w:text/>"),
            &run("Orchard Lane Lease")
        )
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(
        &doc,
        &options("Orchard Lane", "Birch Row", ExpectedMatches::Count(1)),
    );
    let tracked = assert_laws(
        &doc,
        &plan.steps,
        "Re: Orchard Lane Lease",
        "Re: Birch Row Lease",
    );
    // The store holds the control's new text, as Word writes it when it edits a
    // bound control (and as Word keeps the tracked edit only when it does).
    assert!(
        part(&tracked, "docProps/core.xml").contains("<dc:title>Birch Row Lease</dc:title>"),
        "{}",
        part(&tracked, "docProps/core.xml")
    );
    let direct = apply(&doc, &plan.steps, MaterializationMode::Direct);
    assert!(part(&direct, "docProps/core.xml").contains("<dc:title>Birch Row Lease</dc:title>"));
    // Word refills the control from the store on open, so each resolution's
    // store must hold what its control reads.
    for (label, resolved, title) in [
        (
            "Reject All",
            tracked.read_rejected().unwrap(),
            "Orchard Lane Lease",
        ),
        (
            "Accept All",
            tracked.read_accepted().unwrap(),
            "Birch Row Lease",
        ),
    ] {
        let core = part(&resolved, "docProps/core.xml");
        assert!(
            core.contains(&format!("<dc:title>{title}</dc:title>")),
            "{label}: {core}"
        );
    }
}

#[test]
fn a_content_locked_control_refuses_naming_the_lock() {
    for lock in ["contentLocked", "sdtContentLocked"] {
        let body = para(&format!(
            "{}{}",
            run("Orchard tenant: "),
            control(
                &format!(r#"<w:lock w:val="{lock}"/><w:text/>"#),
                &run("Orchard Lane Holdings")
            )
        ));
        let doc = Document::parse(&docx(&body)).unwrap();
        let canonical = doc.snapshot().canonical.clone();
        let result = plan_replace_text(
            &canonical,
            &options("Orchard", "Birch", ExpectedMatches::All),
        );
        assert_eq!(
            refused(result),
            vec![(
                InlineObjectText::ContentControlValue,
                ObjectTextBlock::Locked(lock.to_string())
            )],
            "{lock}"
        );
        let Err(ReplaceTextError::Engine(error)) = plan_replace_text(
            &canonical,
            &options("Orchard", "Birch", ExpectedMatches::All),
        ) else {
            unreachable!()
        };
        assert!(
            error.to_string().contains(&format!("w:lock=\"{lock}\"")),
            "{error}"
        );
    }
}

#[test]
fn an_sdt_locked_control_keeps_its_value_editable() {
    let body = para(&control(
        r#"<w:lock w:val="sdtLocked"/><w:text/>"#,
        &run("Orchard Lane Holdings"),
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let plan = plan(
        &doc,
        &options("Orchard", "Birch", ExpectedMatches::Count(1)),
    );
    assert_laws(
        &doc,
        &plan.steps,
        "Orchard Lane Holdings",
        "Birch Lane Holdings",
    );
}

#[test]
fn a_control_whose_value_is_not_free_text_refuses() {
    let body = para(&control(
        r#"<w:dropDownList><w:listItem w:displayText="Orchard" w:value="1"/></w:dropDownList>"#,
        &run("Orchard"),
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    assert_eq!(
        refused(plan_replace_text(
            &doc.snapshot().canonical,
            &options("Orchard", "Birch", ExpectedMatches::All)
        )),
        vec![(
            InlineObjectText::ContentControlValue,
            ObjectTextBlock::ControlKind("dropDownList".to_string())
        )]
    );
}

#[test]
fn a_replacement_that_would_empty_a_link_refuses() {
    let body = para(&format!("{}{}", run("See "), link(&run("Orchard"))));
    let doc = Document::parse(&docx(&body)).unwrap();
    assert_eq!(
        refused(plan_replace_text(
            &doc.snapshot().canonical,
            &options("Orchard", "", ExpectedMatches::All)
        )),
        vec![(
            InlineObjectText::HyperlinkText,
            ObjectTextBlock::WouldBeEmpty
        )]
    );
}

// ── Several objects in one item ─────────────────────────────────────────

#[test]
fn matches_in_several_objects_and_cells_are_one_item() {
    let body = format!(
        "{}{}<w:p/>",
        para(&format!(
            "{}{}{}{}",
            run("Orchard tenant under "),
            link(&run("Orchard terms")),
            run(" named "),
            control("<w:text/>", &run("Orchard Holdings"))
        )),
        format_args!(
            r#"<w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>"#,
            para(&format!(
                "{}{}",
                run("Signed for "),
                control("", &run("Orchard Holdings"))
            ))
        )
    );
    let doc = Document::parse(&docx(&body)).unwrap();
    // A count that misses one refuses with every site listed.
    match plan_replace_text(
        &doc.snapshot().canonical,
        &options("Orchard", "Birch", ExpectedMatches::Count(3)),
    ) {
        Err(ReplaceTextError::MatchCountMismatch { actual, sites, .. }) => {
            assert_eq!(actual, 4);
            assert_eq!(sites.len(), 4);
        }
        other => panic!("expected a count mismatch, got {other:?}"),
    }
    let plan = plan(
        &doc,
        &options("Orchard", "Birch", ExpectedMatches::Count(4)),
    );
    assert_laws(
        &doc,
        &plan.steps,
        "Orchard tenant under Orchard terms named Orchard Holdings\nSigned for Orchard Holdings\n",
        "Birch tenant under Birch terms named Birch Holdings\nSigned for Birch Holdings\n",
    );
}

#[test]
fn one_locked_object_refuses_the_whole_item() {
    let body = para(&format!(
        "{}{}",
        link(&run("Orchard terms")),
        control(
            r#"<w:lock w:val="contentLocked"/>"#,
            &run("Orchard Holdings")
        )
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    assert_eq!(
        refused(plan_replace_text(
            &doc.snapshot().canonical,
            &options("Orchard", "Birch", ExpectedMatches::All)
        )),
        vec![(
            InlineObjectText::ContentControlValue,
            ObjectTextBlock::Locked("contentLocked".to_string())
        )],
        "the link alone is not replaced while the control keeps its occurrence"
    );
}

// ── Replace all ─────────────────────────────────────────────────────────

fn replace_all(needle: &str, replacement: &str) -> FindReplaceOptions {
    FindReplaceOptions {
        needle: needle.to_string(),
        replacement: replacement.to_string(),
        scope: FindReplaceScope::BodyOnly,
        case_sensitive: true,
        whole_word: false,
        on_barrier_match: BarrierPolicy::Fail,
    }
}

#[test]
fn replace_all_reaches_links_and_controls() {
    let body = para(&format!(
        "{}{}{}{}",
        run("Orchard tenant under "),
        link(&run("Orchard terms")),
        run(" named "),
        control(
            &format!("{TITLE_BINDING}<w:text/>"),
            &run("Orchard Lane Lease")
        )
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let steps = plan_find_replace_all(&doc.snapshot().canonical, &replace_all("Orchard", "Birch"))
        .expect("plans");
    let tracked = assert_laws(
        &doc,
        &steps,
        "Orchard tenant under Orchard terms named Orchard Lane Lease",
        "Birch tenant under Birch terms named Birch Lane Lease",
    );
    assert!(part(&tracked, "docProps/core.xml").contains("<dc:title>Birch Lane Lease</dc:title>"));
}

#[test]
fn replace_all_treats_a_links_edge_as_a_wall_and_refuses_locked_controls() {
    let crossing = para(&format!("{}{}", run("the "), link(&run("Orchard terms"))));
    let doc = Document::parse(&docx(&crossing)).unwrap();
    assert!(matches!(
        plan_find_replace_all(
            &doc.snapshot().canonical,
            &replace_all("the Orchard", "a Birch")
        ),
        Err(EditError::FindReplaceBarrierStraddle { .. })
    ));

    let locked = para(&control(
        r#"<w:lock w:val="contentLocked"/><w:text/>"#,
        &run("Orchard"),
    ));
    let doc = Document::parse(&docx(&locked)).unwrap();
    match plan_find_replace_all(&doc.snapshot().canonical, &replace_all("Orchard", "Birch")) {
        Err(EditError::ReplaceTextInsideInlineObjects { occurrences, .. }) => {
            assert_eq!(
                occurrences[0].reason,
                ObjectTextBlock::Locked("contentLocked".to_string())
            );
        }
        other => panic!("expected the lock refusal, got {other:?}"),
    }
}

// ── A link holding a wrapper its runs cannot carry ──────────────────────

/// A link reading "the Orchard terms on page 4" whose "4" is held by
/// `inner`, inside the link after its plain run.
fn link_holding(inner: &str) -> Document {
    let body = para(&format!(
        "{}{}",
        run("See "),
        link(&format!("{}{inner}", run("the Orchard terms on page ")))
    ));
    Document::parse(&docx(&body)).unwrap()
}

/// Domain rule: a link's text may sit inside a wrapper element within the
/// link: a simple field (§17.16.19 `w:fldSimple`), a content control
/// (§17.5.2.29), a smart tag (§17.5.1.9), a custom XML element (§17.5.1.3),
/// a bidirectional embedding or override (§17.3.2.8, §17.3.2.3), or a nested
/// link. Replacing words in the link edits its runs, and the link is rebuilt
/// from them, which would turn each wrapper into plain runs (a page number
/// field into a frozen "4"). Such a link refuses, naming the wrapper; the
/// replacement is never applied with the wrapper lost.
#[test]
fn a_link_holding_a_run_wrapper_refuses_naming_it() {
    let cases = [
        (
            "fldSimple",
            r#"<w:fldSimple w:instr=" PAGEREF terms \h "><w:r><w:t>4</w:t></w:r></w:fldSimple>"#,
        ),
        (
            "sdt",
            r#"<w:sdt><w:sdtPr><w:id w:val="9"/></w:sdtPr><w:sdtContent><w:r><w:t>4</w:t></w:r></w:sdtContent></w:sdt>"#,
        ),
        (
            "smartTag",
            r#"<w:smartTag w:uri="urn:x" w:element="num"><w:r><w:t>4</w:t></w:r></w:smartTag>"#,
        ),
        (
            "customXml",
            r#"<w:customXml w:uri="urn:x" w:element="num"><w:r><w:t>4</w:t></w:r></w:customXml>"#,
        ),
        (
            "dir",
            r#"<w:dir w:val="rtl"><w:r><w:t>4</w:t></w:r></w:dir>"#,
        ),
        (
            "bdo",
            r#"<w:bdo w:val="rtl"><w:r><w:t>4</w:t></w:r></w:bdo>"#,
        ),
    ];
    for (element, inner) in cases {
        let doc = link_holding(inner);
        assert_eq!(
            refused(plan_replace_text(
                &doc.snapshot().canonical,
                &options("Orchard", "Birch", ExpectedMatches::Count(1)),
            )),
            vec![(
                InlineObjectText::HyperlinkText,
                ObjectTextBlock::WrappedText(element.to_string())
            )],
            "{element}"
        );
        let all =
            plan_find_replace_all(&doc.snapshot().canonical, &replace_all("Orchard", "Birch"));
        assert!(
            matches!(
                &all,
                Err(EditError::ReplaceTextInsideInlineObjects { occurrences, .. })
                    if occurrences.iter().all(|o| o.reason == ObjectTextBlock::WrappedText(element.to_string()))
            ),
            "{element}: replace-all refuses too: {all:?}"
        );
    }
}

/// A complex field (§17.16.18, `w:fldChar` runs) inside a link is carried by
/// the link's runs: a replacement beside it keeps the field.
#[test]
fn a_link_holding_a_complex_field_is_replaced_keeping_it() {
    let doc = link_holding(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>4</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
    );
    let plan = plan(
        &doc,
        &options("Orchard", "Birch", ExpectedMatches::Count(1)),
    );
    let tracked = assert_laws(
        &doc,
        &plan.steps,
        "See the Orchard terms on page 4",
        "See the Birch terms on page 4",
    );
    let accepted = tracked.read_accepted().unwrap();
    let rejected = tracked.read_rejected().unwrap();
    for (label, document) in [
        ("edited", &tracked),
        ("accepted", &accepted),
        ("rejected", &rejected),
    ] {
        let xml = part(document, "word/document.xml");
        assert!(
            xml.contains("<w:instrText xml:space=\"preserve\"> PAGE </w:instrText>"),
            "{label}: {xml}"
        );
    }
}

// ── Straddles are decided on the text as read ───────────────────────────

/// Domain rule: a straddle is an occurrence of the needle in the paragraph
/// as the reader sees it that runs across a wall. A link's text is part of
/// what the reader sees, so text on its two sides is not adjacent: the text
/// "Rent ab", a link "zz", then "cd due" reads "Rent abzzcd due", where "bc"
/// never occurs.
/// Neither planner may refuse (or report a skipped straddle) for it.
#[test]
fn text_on_both_sides_of_a_link_is_not_a_phantom_straddle() {
    let body = para(&format!(
        "{}{}{}",
        run("Rent ab"),
        link(&run("zz")),
        run("cd due")
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let canonical = doc.snapshot().canonical.clone();
    for policy in [BarrierPolicy::Fail, BarrierPolicy::Skip] {
        let mut replace = options("bc", "xy", ExpectedMatches::All);
        replace.on_barrier_match = policy;
        let plan = plan_replace_text(&canonical, &replace)
            .unwrap_or_else(|error| panic!("{policy:?}: replace_text refused: {error:?}"));
        assert!(plan.matches.is_empty(), "{policy:?}");
        assert!(plan.steps.is_empty(), "{policy:?}");
        assert!(
            plan.skipped_straddles.is_empty(),
            "{policy:?}: no straddle to skip: {:?}",
            plan.skipped_straddles
        );

        let mut all = replace_all("bc", "xy");
        all.on_barrier_match = policy;
        let steps = plan_find_replace_all(&canonical, &all)
            .unwrap_or_else(|error| panic!("{policy:?}: replace-all refused: {error:?}"));
        assert!(steps.is_empty(), "{policy:?}: {steps:?}");
    }
}

/// The wall itself still stands: an occurrence that does run across a line
/// break (which reads as no text) is refused by both planners, also in a
/// paragraph that holds a link.
#[test]
fn an_occurrence_across_a_break_beside_a_link_is_still_a_straddle() {
    let body = para(&format!(
        "{}{}{}",
        r#"<w:r><w:t xml:space="preserve">Rent ab</w:t><w:br/><w:t xml:space="preserve">cd due </w:t></w:r>"#,
        link(&run("zz")),
        run(" end")
    ));
    let doc = Document::parse(&docx(&body)).unwrap();
    let canonical = doc.snapshot().canonical.clone();
    assert!(
        matches!(
            plan_replace_text(&canonical, &options("abcd", "xy", ExpectedMatches::All)),
            Err(ReplaceTextError::Engine(
                EditError::FindReplaceBarrierStraddle { .. }
            ))
        ),
        "replace_text"
    );
    assert!(
        matches!(
            plan_find_replace_all(&canonical, &replace_all("abcd", "xy")),
            Err(EditError::FindReplaceBarrierStraddle { .. })
        ),
        "replace-all"
    );
}
