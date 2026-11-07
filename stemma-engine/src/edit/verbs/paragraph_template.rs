//! Character-format inheritance for an explicitly selected paragraph template.
//!
//! Formatting correspondence is not a tracked-change presentation. Match the
//! template's words without redline cleanup: coalescing a large replacement for
//! readability would erase the unchanged boundaries between character styles.
//! The normal paragraph insertion path owns revision materialization.
use super::super::{
    ContentFragment, FormattingContext, InlineIdMint, NewHyperlinkAtom, NewSectionPart,
    NewSectionParts, ParagraphContent, build_text_node_from_exemplar, first_text_formatting,
    normal_segment, normalize_diff_punctuation, placeholder_atom_index,
    synthesize_new_hyperlink_inline,
};
use crate::domain::{BlockNode, InlineNode, NodeId, ParagraphNode, TextNode, TrackedSegment};
use similar::{Algorithm, ChangeTag, TextDiff};

pub(crate) fn content(source: &ParagraphNode, content: &ParagraphContent) -> Vec<TrackedSegment> {
    let mut block = BlockNode::Paragraph(Box::new(source.clone()));
    // Project only an isolated copy. The source's pending history is untouched.
    crate::tracked_model::project_block_for_accept_reject(&mut block, true);
    let BlockNode::Paragraph(template) = block else {
        unreachable!()
    };
    // The template's text runs are its formatting sources. Anything else in it
    // (a bookmark, a field, a link, a picture) is not copied (the new paragraph
    // is built from the caller's content only) and supplies no character
    // formatting, so it is passed over rather than refused.
    let source_runs: Vec<&TextNode> = template
        .all_inlines()
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(&**text),
            _ => None,
        })
        .collect();
    let mut old_text = String::new();
    let mut source_chars = Vec::new();
    for (index, text) in source_runs.iter().enumerate() {
        old_text.push_str(&text.text);
        source_chars.resize(source_chars.len() + text.text.chars().count(), index);
    }
    // The inserted paragraph's links are minted again under its final id when
    // it is placed (`assign_inserted_block_ids`).
    let mut link_ids = InlineIdMint::for_paragraph(source);
    let parts = content
        .fragments
        .iter()
        .map(|fragment| match fragment {
            ContentFragment::Text(text) => NewSectionPart::Text(text.clone()),
            ContentFragment::StyledText { text, marks } => NewSectionPart::StyledText {
                text: text.clone(),
                marks: *marks,
            },
            ContentFragment::NewHyperlink { href, anchor, text } => {
                NewSectionPart::Link(NewHyperlinkAtom {
                    id: link_ids.mint("hyperlink"),
                    href: href.clone(),
                    anchor: anchor.clone(),
                    display: text.clone(),
                })
            }
            ContentFragment::PreservedInlineRef(_) => {
                unreachable!("insert validation rejects preserved references")
            }
        })
        .collect();
    let (new_text, links, authored_marks) = NewSectionParts { parts }.render_for_diff();
    let old_norm = normalize_diff_punctuation(&old_text);
    let new_norm = normalize_diff_punctuation(&new_text);
    let old_tokens = crate::local_change::tokenize(&old_norm);
    let new_tokens = crate::local_change::tokenize(&new_norm);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);
    // A hunk is the run of deleted and inserted tokens between two kept ones.
    // Its new text replaces the template text the hunk deletes, so it takes the
    // formatting most of that text has (ties to the earlier run): a word for a
    // bold word stays bold, while a sentence that replaces "31st" does not pick
    // up the superscript of the ordinal suffix it happened to follow. A hunk
    // that deletes nothing follows the local typing law of explicit text
    // edits: the preceding kept character supplies the format, at the
    // beginning the first source character. None means an empty template, not
    // a failed lookup or an unrecognized state.
    //
    // Whitespace between two changed words is part of the hunk, not kept text
    // that splits it: "within 30 days" replacing "de novo before" is one
    // replacement, not three words each taking the word it lines up with. Kept
    // whitespace is held back until the next change shows which it is.
    //
    // Content that marks any of its text states its own formatting: the caller
    // says which spans are bold, italic, …. Its unchanged words still keep
    // their template formatting, but its new text is the template's body text,
    // not the formatting of the template words it happens to replace: " means"
    // after a marked term does not take the term's bold italic. Body text is
    // the most common run carrying no marks (a reference's authors, not its
    // longer italic title); in a template marked throughout, its most common
    // run.
    let same_format = |a: usize, b: usize| {
        let (a, b) = (source_runs[a], source_runs[b]);
        (&a.marks, &a.style_props, a.rpr_authored) == (&b.marks, &b.style_props, b.rpr_authored)
    };
    let body = if authored_marks.iter().any(Option::is_some) {
        let unmarked: Vec<usize> = source_chars
            .iter()
            .copied()
            .filter(|&run| source_runs[run].marks.is_empty())
            .collect();
        if unmarked.is_empty() {
            dominant_run(&source_chars, same_format)
        } else {
            dominant_run(&unmarked, same_format)
        }
    } else {
        None
    };
    let mut old_cursor = 0;
    let mut inherited = Vec::new();
    let mut hunk: Option<(usize, usize)> = None; // (first deleted char, inserted chars)
    let mut held_space: Option<(usize, usize)> = None; // (first char, chars) of kept whitespace inside a hunk
    let close_hunk =
        |hunk: (usize, usize), old_cursor: usize, inherited: &mut Vec<Option<usize>>| {
            let (start, inserted) = hunk;
            let source_run = if body.is_some() {
                body
            } else if old_cursor > start {
                dominant_run(&source_chars[start..old_cursor], same_format)
            } else if source_chars.is_empty() {
                None
            } else {
                Some(source_chars[start.saturating_sub(1)])
            };
            inherited.resize(inherited.len() + inserted, source_run);
        };
    // Ends the open hunk where held whitespace (if any) begins; that whitespace
    // is then kept text after the hunk.
    let end_hunk = |hunk: &mut Option<(usize, usize)>,
                    held_space: &mut Option<(usize, usize)>,
                    old_cursor: usize,
                    inherited: &mut Vec<Option<usize>>| {
        let Some(open) = hunk.take() else {
            return;
        };
        match held_space.take() {
            Some((start, count)) => {
                close_hunk(open, start, inherited);
                inherited.extend(source_chars[start..start + count].iter().copied().map(Some));
            }
            None => close_hunk(open, old_cursor, inherited),
        }
    };
    for change in diff.iter_all_changes() {
        let count = change.value().chars().count();
        match change.tag() {
            ChangeTag::Equal if hunk.is_some() && change.value().trim().is_empty() => {
                held_space.get_or_insert((old_cursor, 0)).1 += count;
                old_cursor += count;
            }
            ChangeTag::Equal => {
                end_hunk(&mut hunk, &mut held_space, old_cursor, &mut inherited);
                inherited.extend(
                    source_chars[old_cursor..old_cursor + count]
                        .iter()
                        .copied()
                        .map(Some),
                );
                old_cursor += count;
            }
            ChangeTag::Delete | ChangeTag::Insert => {
                // A change after held whitespace: the whitespace was inside the
                // replacement, deleted and inserted with it.
                let open = hunk.get_or_insert((old_cursor, 0));
                if let Some((_, held)) = held_space.take() {
                    open.1 += held;
                }
                if change.tag() == ChangeTag::Delete {
                    old_cursor += count;
                } else {
                    open.1 += count;
                }
            }
        }
    }
    end_hunk(&mut hunk, &mut held_space, old_cursor, &mut inherited);
    assert_eq!(
        old_cursor,
        source_chars.len(),
        "template correspondence omitted source content"
    );
    assert_eq!(
        inherited.len(),
        authored_marks.len(),
        "template correspondence omitted target content"
    );
    let empty_format = first_text_formatting(&template);
    let chars: Vec<_> = new_text.chars().collect();
    let mut cursor = 0;
    let mut inlines = Vec::new();
    while cursor < chars.len() {
        let id = NodeId::from(format!("{}_t{}", source.id.0, inlines.len()));
        let format = match inherited[cursor] {
            Some(index) => {
                let source = source_runs[index];
                FormattingContext {
                    marks: source.marks.clone(),
                    style_props: source.style_props.clone(),
                    rpr_authored: source.rpr_authored,
                }
            }
            None => empty_format.clone(),
        };
        if let Some(index) = placeholder_atom_index(chars[cursor], links.len()) {
            let link = &links[index];
            inlines.push(synthesize_new_hyperlink_inline(
                link.id.clone(),
                &link.href,
                &link.anchor,
                &link.display,
                &format,
            ));
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        while end < chars.len()
            && inherited[end] == inherited[cursor]
            && authored_marks[end] == authored_marks[cursor]
            && placeholder_atom_index(chars[end], links.len()).is_none()
        {
            end += 1;
        }
        // Explicit templating inherits the complete source run context.
        // Session attributes are not node/revision identities; Word can use
        // these physical run boundaries when positioning glyphs. Dropping
        // them can change the rendered result despite identical rPr values.
        // An empty template has no source run context to inherit.
        inlines.push(InlineNode::from(TextNode {
            source_run_attrs: match inherited[cursor] {
                Some(index) => {
                    let mut attributes = source_runs[index].source_run_attrs.clone();
                    // The copied run may later live under a different physical
                    // paragraph mark. Preserve its effective source session,
                    // not an implicit dependency on that destination's default
                    // (§17.3.1.22). Explicit run sessions always take precedence.
                    if !attributes
                        .iter()
                        .any(|(name, _)| name.rsplit(':').next() == Some("rsidR"))
                        && let Some((_, value)) = template
                            .source_paragraph_attrs
                            .iter()
                            .find(|(name, _)| name.rsplit(':').next() == Some("rsidRDefault"))
                    {
                        attributes.push(("w:rsidR".into(), value.clone()));
                    }
                    attributes
                }
                None => Vec::new(),
            },
            ..build_text_node_from_exemplar(
                id,
                &format,
                chars[cursor..end].iter().collect(),
                authored_marks[cursor],
            )
        }));
        cursor = end;
    }
    normal_segment(inlines)
}

/// A source run with the formatting most of `chars` have (`same_format` tells
/// two runs' formatting apart); on a tie, the formatting that comes first.
fn dominant_run(chars: &[usize], same_format: impl Fn(usize, usize) -> bool) -> Option<usize> {
    let mut counts: Vec<(usize, usize)> = Vec::new(); // (a run of that formatting, chars), first-seen order
    for &run in chars {
        match counts
            .iter_mut()
            .find(|(seen, _)| *seen == run || same_format(*seen, run))
        {
            Some((_, n)) => *n += 1,
            None => counts.push((run, 1)),
        }
    }
    counts
        .iter()
        .fold(None, |best: Option<(usize, usize)>, &(run, n)| match best {
            Some((_, most)) if most >= n => best,
            _ => Some((run, n)),
        })
        .map(|(run, _)| run)
}
