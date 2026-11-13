//! Complex fields are kept or replaced whole.
//!
//! A complex field (ISO 29500-1 §17.16.2) is one object spelled as a
//! sequence: a `begin` character, the instruction, a `separate` character,
//! the result and an `end` character (§17.16.18). The engine models each
//! structural part as its own opaque inline, and the token diff reads each
//! as one placeholder character, equal to any other placeholder. A plan can
//! therefore keep a field whose instruction changed (`PAGE` became
//! `NUMPAGES`) as if nothing changed, or keep some of its parts while it
//! deletes or inserts others.
//!
//! The rule: a field is kept only when every one of its structural parts is
//! kept, unchanged, as the matching part of one field on the other side.
//! Its result may then change like any text. Any other field is replaced
//! whole: all of its characters (structure and result) read as deleted, and
//! all of its counterpart's characters read as inserted, the deletion before
//! the insertion. Reject All then restores the old field and Accept All
//! gives the new one.
//!
//! A field part outside a field that opens and closes in the paragraph (a
//! field spanning paragraphs) is left as the plan has it.

use std::ops::Range;

use stemma::domain::{
    FieldKind, InlineChange, InlineChangeSegmentType, InlineNode, OpaqueInlineNode, OpaqueKind,
};
use stemma::local_change::{
    RunFormatting, deleted_token_changes, detect_formatting_change, formatting_for_token_range,
};
use stemma::tracked_model::opaque_payload_changed;

use crate::compiler::{ComparisonText, kept_spelling};

/// One structural part of a field: where its placeholder sits in the
/// comparison text, and the part itself.
struct Part<'a> {
    chars: Range<usize>,
    opaque: &'a OpaqueInlineNode,
}

/// A field that opens and closes in the paragraph: every character from its
/// `begin` through its `end`, and its own structural parts in order (the
/// parts of a field nested in it belong to that field).
struct Field<'a> {
    chars: Range<usize>,
    parts: Vec<Part<'a>>,
}

fn structural_kind(opaque: &OpaqueInlineNode) -> Option<&FieldKind> {
    match &opaque.kind {
        OpaqueKind::Field(data) => match data.field_kind {
            FieldKind::Begin | FieldKind::Instruction | FieldKind::Separate | FieldKind::End => {
                Some(&data.field_kind)
            }
            FieldKind::Simple | FieldKind::Unknown(_) => None,
        },
        _ => None,
    }
}

/// Whether `inlines` hold a structural part of a complex field.
pub(crate) fn has_complex_field_part(inlines: &[InlineNode]) -> bool {
    inlines.iter().any(|inline| {
        matches!(inline, InlineNode::OpaqueInline(opaque) if structural_kind(opaque).is_some())
    })
}

/// Whether a plan whose objects are resolved (each an opaque item, compared
/// by identity) keeps some structural parts of a field and deletes or
/// inserts others. Read on each side: the base's field from its kept and
/// deleted items, the target's from its kept and inserted items. A field
/// that does not close in the paragraph is not judged.
pub(crate) fn plan_tears_a_field(plan: &[InlineChange]) -> bool {
    // One frame per open field on each side: whether any part was kept, and
    // whether any was changed.
    let mut base: Vec<(bool, bool)> = Vec::new();
    let mut target: Vec<(bool, bool)> = Vec::new();
    for change in plan {
        let InlineChange::Opaque {
            segment_type,
            field_kind: Some(kind),
            ..
        } = change
        else {
            continue;
        };
        let kept = *segment_type == InlineChangeSegmentType::Equal;
        for (stack, on_side) in [
            (&mut base, *segment_type != InlineChangeSegmentType::Insert),
            (
                &mut target,
                *segment_type != InlineChangeSegmentType::Delete,
            ),
        ] {
            if !on_side {
                continue;
            }
            match kind {
                FieldKind::Begin => stack.push((kept, !kept)),
                FieldKind::Instruction | FieldKind::Separate | FieldKind::End => {
                    if let Some(frame) = stack.last_mut() {
                        frame.0 |= kept;
                        frame.1 |= !kept;
                        if *frame == (true, true) {
                            return true;
                        }
                    }
                    if *kind == FieldKind::End {
                        stack.pop();
                    }
                }
                FieldKind::Simple | FieldKind::Unknown(_) => {}
            }
        }
    }
    false
}

/// The fields of a paragraph's inlines that open and close in it, measured
/// in `width`'s characters (the comparison text's measure).
fn fields<'a>(inlines: &'a [InlineNode], width: &dyn Fn(&InlineNode) -> usize) -> Vec<Field<'a>> {
    let mut open: Vec<Field<'a>> = Vec::new();
    let mut closed = Vec::new();
    let mut at = 0usize;
    for inline in inlines {
        let len = width(inline);
        if let InlineNode::OpaqueInline(opaque) = inline
            && let Some(kind) = structural_kind(opaque)
        {
            let part = Part {
                chars: at..at + len,
                opaque,
            };
            match kind {
                FieldKind::Begin => open.push(Field {
                    chars: at..at + len,
                    parts: vec![part],
                }),
                FieldKind::End => {
                    if let Some(mut field) = open.pop() {
                        field.parts.push(part);
                        field.chars.end = at + len;
                        closed.push(field);
                    }
                }
                _ => {
                    if let Some(field) = open.last_mut() {
                        field.parts.push(part);
                    }
                }
            }
        }
        at += len;
    }
    closed
}

/// Keep every complex field of a token-diff plan whole or replace it whole
/// (see the module documentation). A plan that already does is returned as
/// it is.
///
/// `plan` is the token-diff plan of `old` against `new` over their comparison
/// texts `old_text` / `new_text`, with no opaque items. Its changes write
/// stored characters ([`ComparisonText::source`]); fields are found and
/// judged in comparison characters.
pub(crate) fn keep_or_replace_fields_whole(
    old: &[InlineNode],
    new: &[InlineNode],
    old_text: &ComparisonText,
    new_text: &ComparisonText,
    width: &dyn Fn(&InlineNode) -> usize,
    plan: Vec<InlineChange>,
) -> Vec<InlineChange> {
    let (old_fmt, new_fmt) = (&old_text.char_fmt, &new_text.char_fmt);
    let old_fields = fields(old, width);
    let new_fields = fields(new, width);
    if old_fields.is_empty() && new_fields.is_empty() {
        return plan;
    }

    // The plan's alignment: which new character each kept old character is.
    let mut old_to_new: Vec<Option<usize>> = vec![None; old_fmt.len()];
    let (mut old_at, mut new_at) = (0usize, 0usize);
    for change in &plan {
        match change {
            InlineChange::Unchanged { .. } => {
                for _ in 0..width_of(change, old_text, new_text, old_at, new_at) {
                    old_to_new[old_at] = Some(new_at);
                    old_at += 1;
                    new_at += 1;
                }
            }
            InlineChange::Deleted { .. } => {
                old_at += width_of(change, old_text, new_text, old_at, new_at);
            }
            InlineChange::Inserted { .. } => {
                new_at += width_of(change, old_text, new_text, old_at, new_at);
            }
            InlineChange::Opaque { .. } => {
                unreachable!("a token-diff plan holds no opaque items")
            }
        }
    }
    assert_eq!(
        (old_at, new_at),
        (old_fmt.len(), new_fmt.len()),
        "a token-diff plan spells both texts"
    );

    // A pair of fields is kept when each part of the old one is kept as the
    // same part, unchanged, of the new one, and the two have the same parts.
    let kept_as = |field: &Field, other: &Field| {
        field.parts.len() == other.parts.len()
            && field
                .parts
                .iter()
                .zip(&other.parts)
                .all(|(part, counterpart)| {
                    part.chars.len() == counterpart.chars.len()
                        && part
                            .chars
                            .clone()
                            .zip(counterpart.chars.clone())
                            .all(|(o, n)| old_to_new[o] == Some(n))
                        && structural_kind(part.opaque) == structural_kind(counterpart.opaque)
                        && !opaque_payload_changed(part.opaque, counterpart.opaque)
                })
    };
    let mut new_kept = vec![false; new_fields.len()];
    let mut old_replaced = vec![false; old_fmt.len()];
    for field in &old_fields {
        let counterpart = old_to_new[field.chars.start].and_then(|begin| {
            new_fields
                .iter()
                .position(|other| other.chars.start == begin)
        });
        match counterpart {
            Some(index) if kept_as(field, &new_fields[index]) => new_kept[index] = true,
            _ => old_replaced[field.chars.clone()].fill(true),
        }
    }
    let mut new_replaced = vec![false; new_fmt.len()];
    for (field, kept) in new_fields.iter().zip(&new_kept) {
        if !kept {
            new_replaced[field.chars.clone()].fill(true);
        }
    }
    let broken = |o: usize, n: usize| old_replaced[o] || new_replaced[n];
    if !old_to_new
        .iter()
        .enumerate()
        .any(|(o, n)| n.is_some_and(|n| broken(o, n)))
    {
        return plan;
    }

    // Rebuild: a kept character stays kept unless it is in a replaced field;
    // each run of changes that touches a replaced field reads as all its
    // deletions, then all its insertions.
    let mut out = Vec::new();
    let mut deleted: Vec<InlineChange> = Vec::new();
    let mut inserted: Vec<InlineChange> = Vec::new();
    let mut touched = false;
    let mut order: Vec<InlineChange> = Vec::new();
    let flush = |out: &mut Vec<InlineChange>,
                 deleted: &mut Vec<InlineChange>,
                 inserted: &mut Vec<InlineChange>,
                 order: &mut Vec<InlineChange>,
                 touched: &mut bool| {
        if *touched {
            out.append(deleted);
            out.append(inserted);
        } else {
            out.append(order);
        }
        deleted.clear();
        inserted.clear();
        order.clear();
        *touched = false;
    };
    let old_chars: Vec<char> = old_text.text.chars().collect();
    let (mut old_at, mut new_at) = (0usize, 0usize);
    for change in plan {
        match &change {
            InlineChange::Unchanged { .. } => {
                let width = width_of(&change, old_text, new_text, old_at, new_at);
                let chars = &old_chars[old_at..old_at + width];
                if !(0..chars.len()).any(|i| broken(old_at + i, new_at + i)) {
                    flush(
                        &mut out,
                        &mut deleted,
                        &mut inserted,
                        &mut order,
                        &mut touched,
                    );
                    old_at += chars.len();
                    new_at += chars.len();
                    out.push(change);
                    continue;
                }
                let mut i = 0usize;
                while i < chars.len() {
                    let is_broken = broken(old_at + i, new_at + i);
                    let mut j = i + 1;
                    while j < chars.len() && broken(old_at + j, new_at + j) == is_broken {
                        j += 1;
                    }
                    let piece: String = chars[i..j].iter().collect();
                    let (o, n, len) = (old_at + i, new_at + i, j - i);
                    if is_broken {
                        touched = true;
                        let mut at = o;
                        for mut part in deleted_token_changes(&piece, old_fmt, o) {
                            if let InlineChange::Deleted { text, .. } = &mut part {
                                let part_len = text.chars().count();
                                *text = old_text.source(at, part_len);
                                at += part_len;
                            }
                            deleted.push(part);
                        }
                        let fmt = formatting_for_token_range(new_fmt, n, len);
                        inserted.push(InlineChange::Inserted {
                            text: new_text.source(n, len),
                            marks: fmt.marks,
                            style_props: fmt.style_props,
                            rpr_authored: fmt.rpr_authored,
                            formatting_change: None,
                            rev_id: 0,
                        });
                    } else {
                        flush(
                            &mut out,
                            &mut deleted,
                            &mut inserted,
                            &mut order,
                            &mut touched,
                        );
                        let old_piece = formatting_for_token_range(old_fmt, o, len);
                        let new_piece = formatting_for_token_range(new_fmt, n, len);
                        // As the token diff writes kept text.
                        let text = kept_spelling(old_text, o, new_text, n, len);
                        let (marks, style_props, rpr_authored, formatting_change) =
                            detect_formatting_change(
                                &old_piece,
                                &new_piece,
                                RunFormatting::AsWritten,
                                None,
                            );
                        out.push(InlineChange::Unchanged {
                            text,
                            marks,
                            style_props,
                            rpr_authored,
                            formatting_change,
                        });
                    }
                    i = j;
                }
                old_at += chars.len();
                new_at += chars.len();
            }
            InlineChange::Deleted { .. } => {
                let len = width_of(&change, old_text, new_text, old_at, new_at);
                touched |= old_replaced[old_at..old_at + len].iter().any(|&r| r);
                old_at += len;
                deleted.push(change.clone());
                order.push(change);
            }
            InlineChange::Inserted { .. } => {
                let len = width_of(&change, old_text, new_text, old_at, new_at);
                touched |= new_replaced[new_at..new_at + len].iter().any(|&r| r);
                new_at += len;
                inserted.push(change.clone());
                order.push(change);
            }
            InlineChange::Opaque { .. } => {
                unreachable!("a token-diff plan holds no opaque items")
            }
        }
    }
    flush(
        &mut out,
        &mut deleted,
        &mut inserted,
        &mut order,
        &mut touched,
    );
    out
}

/// How many comparison characters a plan change spans, starting at comparison
/// offsets `old_at` / `new_at`: a change writes stored characters, and one
/// stored character can render as several comparison characters (its source
/// on the first, "" on the rest). A kept change is spelled as either side
/// wrote it.
///
/// # Panics
///
/// When the change's text is not the stored characters at its place: the
/// plan does not spell its texts, a bug in the token diff.
fn width_of(
    change: &InlineChange,
    old_text: &ComparisonText,
    new_text: &ComparisonText,
    old_at: usize,
    new_at: usize,
) -> usize {
    let width = match change {
        InlineChange::Unchanged { text, .. } => stored_span(&old_text.source, old_at, text)
            .or_else(|| stored_span(&new_text.source, new_at, text)),
        InlineChange::Deleted { text, .. } => stored_span(&old_text.source, old_at, text),
        InlineChange::Inserted { text, .. } => stored_span(&new_text.source, new_at, text),
        InlineChange::Opaque { .. } => unreachable!("a token-diff plan holds no opaque items"),
    };
    width.unwrap_or_else(|| {
        panic!("a token-diff plan change is not the stored text at its place: {change:?}")
    })
}

/// The number of comparison characters from `at` whose stored characters
/// spell `stored`, with the characters a stored one renders as kept whole.
fn stored_span(source: &[String], at: usize, stored: &str) -> Option<usize> {
    let mut end = at;
    let mut bytes = 0usize;
    while bytes < stored.len() {
        bytes += source.get(end)?.len();
        end += 1;
    }
    while source.get(end).is_some_and(String::is_empty) {
        end += 1;
    }
    (source[at..end].concat() == stored).then_some(end - at)
}
