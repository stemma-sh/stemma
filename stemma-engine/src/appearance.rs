//! Appearance equality: the reading of "untouched" for a document another
//! program has saved.
//!
//! The fidelity comparison (`roundtrip_compare`) asks whether two blocks are
//! the same markup: it is the right question for a document Stemma itself
//! round-trips, where even a revision-save id must survive. A program that
//! rewrites the package (a word processor's Save, a comparer, a packer that
//! merges runs) changes that markup everywhere without changing what the
//! document says or how it looks. Asking the fidelity question of its output
//! reports every block as altered.
//!
//! [`for_appearance`] puts a block into a form in which two blocks that read
//! and look the same compare equal under the fidelity comparison. It removes
//! only differences that are not visible and carry no content:
//!
//! - bookkeeping: revision-save ids on paragraphs, runs and rows, and the
//!   records of where a property was written (on the element or inherited),
//!   which the model keeps for exact round trips. The model holds the
//!   resolved value of each property either way, and that value is compared;
//! - run boundaries: adjacent text with identical formatting and the same
//!   tracked state is one stretch of text, however many runs spell it;
//! - a default written out: no paragraph style is the document's default
//!   paragraph style, and so is a reference to a style the document does not
//!   define (§17.3.1.27); spacing left unset is zero before and after, and
//!   single line spacing;
//! - what marks or holds nothing: a proofing mark, a run with properties and
//!   no content outside a tracked change, an empty property element, a
//!   bookmark marker of a bookmark that does not exist
//!   ([`bookmarks_in_effect`]);
//! - the spelling of held XML: attribute order, the order of a property
//!   element's children, a content control's numeric id, white space in
//!   base64 graphic data; a held hyperlink run is read as one run per piece
//!   of its content.
//!
//! The audit adds one reading of its own under appearance equality: notes
//! renumbered by a save are compared by their references' order
//! (`audit::note_renames`).
//!
//! Everything else is compared as before: text, resolved formatting values,
//! tracked changes, structure, fields, drawings.

use crate::domain::{
    BlockNode, InlineNode, ParagraphNode, ParagraphSpacing, RunRprAuthored, TextNode, TrackedBlock,
    TrackedSegment,
};

const WORDPROCESSINGML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const VML_OFFICE: &str = "urn:schemas-microsoft-com:office:office";
const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Per source part of a package, what each of its relationships points at
/// (`runtime::relationship_targets`).
pub(crate) type PartRelationships =
    std::collections::HashMap<String, std::collections::HashMap<String, String>>;

/// The key under which [`PartRelationships`] also lists the main document
/// part, whatever its name in the package.
pub(crate) const MAIN_PART: &str = "\u{0}main";

/// What the comparison takes from a package beyond the document model.
#[derive(Default)]
pub(crate) struct PackageFacts {
    /// The main document part's name in the package.
    pub(crate) main_part: String,
    /// Per source part, what each of its relationships points at.
    pub(crate) relationships: PartRelationships,
    /// Per list reference a paragraph can carry (`w:numId`, `w:ilvl`), a
    /// digest of the level definition it resolves to in the numbering part:
    /// the number format, the marker text, the start value and the level's
    /// own properties. Two references with equal digests number and mark
    /// their paragraphs the same way, whatever their ids.
    pub(crate) numbering_levels: std::collections::HashMap<(u32, u32), String>,
    /// The document settings that bear on how it reads or looks
    /// ([`settings_in_effect`]), each as (name, canonical XML), by name.
    pub(crate) settings: Vec<(String, String)>,
    /// Per theme colour a run can name (`w:themeColor`, §17.18.97), the
    /// colour the theme's colour scheme gives it ([`theme_colors`]).
    pub(crate) theme_colors: std::collections::HashMap<String, String>,
    /// The theme entries the document's content uses, with what the theme
    /// gives each ([`theme_in_use`]), by name.
    pub(crate) theme_in_use: Vec<(String, String)>,
    /// What the theme part is ([`part_digest`]); `theme_in_use` is read only
    /// when two packages' themes differ.
    pub(crate) theme_digest: Option<String>,
    /// What the styles part is ([`part_digest`]): where two packages' styles
    /// parts are the same, nothing their text takes from them differs.
    pub(crate) styles_digest: Option<String>,
    /// The table styles of the styles part ([`table_styles`]).
    pub(crate) table_styles: TableStyles,
    /// Per paragraph style, the paragraph properties its chain writes
    /// ([`paragraph_style_properties`]).
    pub(crate) paragraph_styles: std::collections::HashMap<String, String>,
    /// The note separators ([`note_separators`]).
    pub(crate) note_separators: Vec<(String, String)>,
    /// The styles `STYLEREF` fields name, resolved ([`styleref_targets`]);
    /// read only where two packages' styles or themes differ.
    pub(crate) styleref_targets: Vec<(String, String)>,
    /// What each data binding of the content controls reads in its store
    /// (`data_binding::bound_values`): Word fills bound controls from it on
    /// open.
    pub(crate) bound_values: Vec<crate::data_binding::BoundValue>,
    /// The fonts the font table embeds (`w:embedRegular` and the like,
    /// §17.8.3.3), as `font name/face`.
    pub(crate) embedded_fonts: Vec<String>,
}

/// What comparing a block needs from the block's own document.
#[derive(Clone, Copy)]
pub(crate) struct Side<'a> {
    /// The document's styles, when it has a styles part.
    pub(crate) styles: Option<&'a crate::styles::StyleDefinitions>,
    /// The bookmark markers of the document that mark something, by node id
    /// ([`bookmarks_in_effect`]). Any other bookmark marker marks nothing
    /// and is left out. `None`: markers compared as held.
    pub(crate) bookmarks: Option<&'a std::collections::HashSet<String>>,
    /// The package's facts, when the caller holds the package.
    pub(crate) parts: Option<&'a PackageFacts>,
    /// What each relationship of the part holding the block points at. A
    /// reference inside a held element is compared by this. `None` (the
    /// package or the part is not known): compared by id.
    pub(crate) relationships: Option<&'a std::collections::HashMap<String, String>>,
}

impl<'a> Side<'a> {
    /// The id of the default paragraph style.
    fn default_paragraph_style(self) -> Option<&'a str> {
        self.styles?.default_para_style_id()
    }

    /// This side, for blocks of `story`. A header or footer is found by its
    /// part path; notes and comments by their conventional part names.
    pub(crate) fn for_story(self, story: &crate::domain::StoryScope) -> Side<'a> {
        use crate::domain::StoryScope;
        let lookup = |part: &str| -> Option<&'a std::collections::HashMap<String, String>> {
            let parts = &self.parts?.relationships;
            // A part path written absolute names the package path itself.
            match part.strip_prefix('/') {
                Some(absolute) => parts.get(absolute),
                None => parts
                    .get(part)
                    .or_else(|| parts.get(&format!("word/{part}"))),
            }
        };
        let relationships = match story {
            StoryScope::Body => lookup(MAIN_PART),
            StoryScope::Header { part_path, .. } | StoryScope::Footer { part_path, .. } => {
                lookup(part_path)
            }
            StoryScope::Footnote { .. } => lookup("word/footnotes.xml"),
            StoryScope::Endnote { .. } => lookup("word/endnotes.xml"),
            StoryScope::Comment { .. } => lookup("word/comments.xml"),
            // A text frame's content is held in its host's element, which is
            // compared with the host's story.
            StoryScope::TextFrame { .. } => None,
        };
        Side {
            relationships,
            ..self
        }
    }
}

/// `block` in appearance form.
pub(crate) fn for_appearance(block: &TrackedBlock, side: Side<'_>) -> TrackedBlock {
    let mut block = block.clone();
    normalize_block(&mut block.block, side);
    if let Some(control) = &mut block.block_sdt_wrap {
        canonical_block_control(control);
    }
    block
}

/// A block content control's held properties (`w:sdtPr`, `w:sdtEndPr`),
/// its own and its nested controls', in one spelling and without the
/// control's numeric id, as a held element's are.
fn canonical_block_control(control: &mut crate::domain::BlockSdtWrap) {
    let canonical = |raw: &mut Vec<u8>| {
        if let Ok(mut element) = crate::word_xml::parse_raw_fragment(raw) {
            strip_bookkeeping(&mut element);
            strip_identities(&mut element);
            sort_attributes(&mut element);
            *raw = crate::word_xml::serialize_raw_fragment(&element);
        }
    };
    canonical(&mut control.wrapper.sdt_pr_xml);
    if let Some(end) = &mut control.wrapper.sdt_end_pr_xml {
        canonical(end);
    }
    for nested in &mut control.nested {
        canonical_block_control(&mut nested.control);
    }
}

/// `block` with every relationship reference inside a held element joined
/// to what it points at, and nothing else changed: the form in which the
/// fidelity comparison sees that an image was replaced under an unchanged
/// id. Without relationships for the block's part, the block as it is.
pub(crate) fn with_relationship_targets(block: &TrackedBlock, side: Side<'_>) -> TrackedBlock {
    let mut block = block.clone();
    read_text_in_held_content(&mut block.block);
    if side.relationships.is_some() {
        bind_targets(&mut block.block, side);
    }
    bind_theme_colors(&mut block.block, side);
    block
}

/// Every run property set of `block`: the paragraph mark's, a label's, each
/// inline's, and the previous properties of tracked changes to them.
fn for_each_style_props(block: &mut BlockNode, f: &mut dyn FnMut(&mut crate::domain::StyleProps)) {
    fn change(
        change: &mut Option<crate::domain::FormattingChange>,
        f: &mut dyn FnMut(&mut crate::domain::StyleProps),
    ) {
        if let Some(change) = change {
            f(&mut change.previous_style_props);
        }
    }
    match block {
        BlockNode::Paragraph(paragraph) => {
            f(&mut paragraph.paragraph_mark_style_props);
            f(&mut paragraph.literal_prefix_style_props);
            for carrier in [
                &mut paragraph.literal_prefix_leading_rpr,
                &mut paragraph.literal_prefix_trailing_rpr,
            ]
            .into_iter()
            .flatten()
            {
                f(&mut carrier.style_props);
                for run in &mut carrier.source_runs {
                    f(&mut run.style_props);
                }
            }
            if let Some(formatting) = &mut paragraph.formatting_change {
                f(&mut formatting.previous.paragraph_mark.style_props);
            }
            for inline in paragraph.segments.iter_mut().flat_map(|s| &mut s.inlines) {
                match inline {
                    InlineNode::Text(text) => {
                        f(&mut text.style_props);
                        change(&mut text.formatting_change, f);
                    }
                    InlineNode::OpaqueInline(opaque) => {
                        f(&mut opaque.wrapper_style_props);
                        change(&mut opaque.formatting_change, f);
                    }
                    InlineNode::Decoration(decoration) => {
                        f(&mut decoration.wrapper_style_props);
                        change(&mut decoration.formatting_change, f);
                    }
                    InlineNode::HardBreak(hard_break) => {
                        f(&mut hard_break.wrapper_style_props);
                        change(&mut hard_break.formatting_change, f);
                    }
                    InlineNode::CommentRangeStart { .. } | InlineNode::CommentRangeEnd { .. } => {}
                }
            }
        }
        BlockNode::Table(table) => {
            for block in table
                .rows
                .iter_mut()
                .flat_map(|row| &mut row.cells)
                .flat_map(|cell| &mut cell.blocks)
            {
                for_each_style_props(block, f);
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Each theme colour a run names (`w:themeColor`) joined to the colour the
/// side's theme gives it (`?` when the theme does not), so that a colour
/// changed in the theme part is a changed colour of the runs that use it.
/// Without the package, the block as it is.
fn bind_theme_colors(block: &mut BlockNode, side: Side<'_>) {
    let Some(facts) = side.parts else {
        return;
    };
    for_each_style_props(block, &mut |props| {
        if let Some(reference) = &mut props.color_theme {
            let colour = facts
                .theme_colors
                .get(reference.theme_color.as_ref())
                .map(String::as_str)
                .unwrap_or("?");
            reference.theme_color = format!("{}={colour}", reference.theme_color).into();
        }
    });
}

fn bind_targets(block: &mut BlockNode, side: Side<'_>) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for segment in &mut paragraph.segments {
                for inline in &mut segment.inlines {
                    if let InlineNode::OpaqueInline(opaque) = inline
                        && opaque.content_hash.is_some()
                        && let Some(raw) = &opaque.raw_xml
                        && let Ok(mut element) = crate::word_xml::parse_raw_fragment(raw)
                        && rewrite_references(&mut element, side, true)
                    {
                        opaque.content_hash = crate::import::opaque_content_hash_of(
                            &opaque.kind,
                            &crate::word_xml::serialize_raw_fragment(&element),
                        );
                    }
                }
            }
        }
        BlockNode::Table(table) => {
            for row in &mut table.rows {
                for cell in &mut row.cells {
                    for block in &mut cell.blocks {
                        bind_targets(block, side);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Replace each relationship reference (`r:id`, `r:embed`, `r:link`,
/// `r:pict`) the side knows by what it points at; with `keep_id`, by the id
/// and what it points at. Returns whether any reference was rewritten.
fn rewrite_references(element: &mut xmltree::Element, side: Side<'_>, keep_id: bool) -> bool {
    let mut rewritten = false;
    if let Some(relationships) = side.relationships {
        for (name, value) in element.attributes.iter_mut() {
            if name.prefix.as_deref() == Some("r")
                && matches!(name.local_name.as_str(), "id" | "embed" | "link" | "pict")
                && let Some(target) = relationships.get(value.as_str())
            {
                *value = if keep_id {
                    format!("{value}={target}")
                } else {
                    target.clone()
                };
                rewritten = true;
            }
        }
    }
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            rewritten |= rewrite_references(child, side, keep_id);
        }
    }
    rewritten
}

/// The bookmark markers of `document` that mark something, as the node ids
/// of their decorations (and of block-level marker blocks): the start of a
/// bookmark that has a matching end after it (a start without one makes the
/// document non-conformant and marks nothing, §17.13.6.2), the first by its
/// start's place in the document of the bookmarks sharing a name (the later
/// ones are ignored, §17.13.6.2 `name`), and the end that closes each such
/// start. Stories are taken body first, then headers, footers, footnotes,
/// endnotes and comments. Markers are taken one by one, not by id: a writer
/// may give two bookmarks one id.
///
/// A bookmark marks a place; it is not displayed. Two documents with the
/// same names at the same places read and look the same, whatever ids and
/// dead markers either spells them with.
pub(crate) fn bookmarks_in_effect(
    document: &crate::domain::CanonDoc,
) -> std::collections::HashSet<String> {
    // (node id, bookmark id, name for a start / None for an end), in
    // document order.
    type Marker = (String, String, Option<String>);
    let mut markers: Vec<Marker> = Vec::new();
    fn walk(block: &BlockNode, markers: &mut Vec<Marker>) {
        match block {
            BlockNode::Paragraph(paragraph) => {
                for inline in paragraph.segments.iter().flat_map(|s| &s.inlines) {
                    if let InlineNode::Decoration(decoration) = inline
                        && decoration.kind == crate::domain::DecorationType::Bookmark
                        && let Some((id, name)) =
                            decoration.raw_xml.as_deref().and_then(bookmark_marker)
                    {
                        markers.push((decoration.id.to_string(), id, name));
                    }
                }
            }
            BlockNode::Table(table) => {
                for block in table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .flat_map(|cell| &cell.blocks)
                {
                    walk(block, markers);
                }
            }
            BlockNode::OpaqueBlock(opaque) => {
                if let Some(marker) = &opaque.range_marker
                    && marker.family == crate::domain::RangeMarkerFamily::Bookmark
                    && let Some((id, name)) = bookmark_marker(&marker.raw_xml)
                {
                    markers.push((opaque.id.to_string(), id, name));
                }
            }
        }
    }
    let stories = std::iter::once(&document.blocks)
        .chain(document.headers.iter().map(|story| &story.blocks))
        .chain(document.footers.iter().map(|story| &story.blocks))
        .chain(document.footnotes.iter().map(|story| &story.blocks))
        .chain(document.endnotes.iter().map(|story| &story.blocks))
        .chain(document.comments.iter().map(|story| &story.blocks));
    for blocks in stories {
        for block in blocks {
            walk(&block.block, &mut markers);
        }
    }
    // Per bookmark id, the places of its ends, in order.
    let mut ends: std::collections::HashMap<&str, std::collections::VecDeque<usize>> =
        std::collections::HashMap::new();
    for (position, (_, id, name)) in markers.iter().enumerate() {
        if name.is_none() {
            ends.entry(id).or_default().push_back(position);
        }
    }
    let mut names_taken = std::collections::HashSet::new();
    let mut in_effect = std::collections::HashSet::new();
    for (position, (node, id, name)) in markers.iter().enumerate() {
        let Some(name) = name else { continue };
        let Some(queue) = ends.get_mut(id.as_str()) else {
            continue;
        };
        // The end that closes this start: the first unused one after it.
        while queue.front().is_some_and(|end| *end < position) {
            queue.pop_front();
        }
        let Some(end) = queue.front().copied() else {
            continue;
        };
        if names_taken.insert(name.clone()) {
            queue.pop_front();
            in_effect.insert(node.clone());
            in_effect.insert(markers[end].0.clone());
        }
    }
    in_effect
}

/// A bookmark marker's id, and its name when it is a start.
fn bookmark_marker(raw: &[u8]) -> Option<(String, Option<String>)> {
    let element = crate::word_xml::parse_raw_fragment(raw).ok()?;
    let attribute = |local: &str| {
        element
            .attributes
            .iter()
            .find(|(name, _)| {
                name.local_name == local && name.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .map(|(_, value)| value.clone())
    };
    let id = attribute("id")?;
    match element.name.as_str() {
        // A start without its required name (§17.13.6.2) is not read here;
        // the comparison keeps it as written.
        "bookmarkStart" => Some((id, Some(attribute("name")?))),
        "bookmarkEnd" => Some((id, None)),
        _ => None,
    }
}

/// Every block of `document`, body and stories, in appearance form, and its
/// section properties without revision-save ids.
pub(crate) fn document_for_appearance(document: &mut crate::domain::CanonDoc, side: Side<'_>) {
    use crate::domain::StoryScope;
    let normalize = |blocks: &mut Vec<TrackedBlock>, story: StoryScope| {
        let side = side.for_story(&story);
        for block in blocks {
            normalize_block(&mut block.block, side);
            if let Some(control) = &mut block.block_sdt_wrap {
                canonical_block_control(control);
            }
        }
    };
    normalize(&mut document.blocks, StoryScope::Body);
    for story in &mut document.headers {
        let scope = StoryScope::Header {
            part_path: story.part_name.clone(),
            kind: story.kind.clone(),
        };
        normalize(&mut story.blocks, scope);
    }
    for story in &mut document.footers {
        let scope = StoryScope::Footer {
            part_path: story.part_name.clone(),
            kind: story.kind.clone(),
        };
        normalize(&mut story.blocks, scope);
    }
    for story in &mut document.footnotes {
        normalize(
            &mut story.blocks,
            StoryScope::Footnote {
                id: story.id.clone(),
            },
        );
    }
    for story in &mut document.endnotes {
        normalize(
            &mut story.blocks,
            StoryScope::Endnote {
                id: story.id.clone(),
            },
        );
    }
    for story in &mut document.comments {
        normalize(
            &mut story.blocks,
            StoryScope::Comment {
                id: story.id.clone(),
            },
        );
    }
    if let Some(section) = &mut document.body_section_properties {
        section.source_section_attrs.clear();
    }
}

fn normalize_block(block: &mut BlockNode, side: Side<'_>) {
    bind_theme_colors(block, side);
    normalize_block_spelling(block, side);
}

fn normalize_block_spelling(block: &mut BlockNode, side: Side<'_>) {
    match block {
        BlockNode::Paragraph(paragraph) => normalize_paragraph(paragraph, side),
        BlockNode::Table(table) => {
            canonical_preserved(&mut table.formatting.preserved);
            // A grid history (`w:tblGridChange`, §17.13.5.33) that no pending
            // table, row or cell property change and no pending row or cell
            // owns is auxiliary: it has no author or date, and Word discards
            // it on Accept and on Reject alike, keeping the current grid.
            let pending_structure = table.rows.iter().any(|row| {
                row.tracking_status
                    .iter()
                    .chain(
                        row.cells
                            .iter()
                            .filter_map(|cell| cell.tracking_status.as_ref()),
                    )
                    .any(|status| !matches!(status, crate::domain::TrackingStatus::Normal))
            });
            if !pending_structure
                && crate::tracked_model::table_property_identities(table).is_empty()
            {
                table.formatting.grid_history = None;
            }
            if let Some(change) = &mut table.formatting_change {
                canonical_preserved(&mut change.previous.preserved);
            }
            for row in &mut table.rows {
                row.source_row_attrs.clear();
                row.para_id = None;
                row.text_id = None;
                canonical_preserved(&mut row.preserved);
                if let Some(change) = &mut row.formatting_change {
                    canonical_preserved(&mut change.previous_preserved);
                }
                for cell in &mut row.cells {
                    canonical_preserved(&mut cell.preserved);
                    if let Some(change) = &mut cell.formatting_change {
                        canonical_preserved(&mut change.previous_preserved);
                    }
                    for block in &mut cell.blocks {
                        normalize_block_spelling(block, side);
                    }
                }
            }
        }
        // Held as bytes; nothing in it is modeled, so nothing is normalized.
        BlockNode::OpaqueBlock(_) => {}
    }
}

fn normalize_paragraph(paragraph: &mut ParagraphNode, side: Side<'_>) {
    paragraph.source_paragraph_attrs.clear();
    if let Some(section) = &mut paragraph.section_properties {
        section.source_section_attrs.clear();
    }
    // A reference to a style the document does not define applies no style
    // (§17.3.1.27), as no reference does. The heading level the model read
    // from the undefined id's name goes with it; the level is then what the
    // paragraph's own outline level and the default style give.
    if let Some(style_id) = &paragraph.style_id
        && !side
            .styles
            .is_some_and(|styles| styles.defines_paragraph_style(style_id))
    {
        paragraph.style_id = None;
        paragraph.heading_level = crate::import::heading_level_number(
            None,
            side.default_paragraph_style(),
            paragraph.outline_lvl,
            side.styles,
        )
        .map(crate::domain::HeadingLevel::from_number);
    }
    if paragraph.style_id.is_none()
        && let Some(default) = side.default_paragraph_style()
    {
        paragraph.style_id = Some(default.into());
    }

    // Where a property was written is bookkeeping; its resolved value stays.
    paragraph.has_direct_align = true;
    paragraph.has_direct_indent = true;
    paragraph.has_direct_spacing = true;
    paragraph.has_direct_keep_next = true;
    paragraph.has_direct_keep_lines = true;
    paragraph.has_direct_page_break_before = true;
    paragraph.has_direct_widow_control = true;
    paragraph.has_direct_contextual_spacing = true;
    paragraph.has_direct_shading = true;
    paragraph.has_direct_borders = true;
    paragraph.authored_indent = None;
    paragraph.authored_spacing = None;

    paragraph.spacing = Some(spacing_with_defaults(paragraph.spacing.take()));
    canonical_preserved(&mut paragraph.preserved_ppr);
    canonical_preserved(&mut paragraph.paragraph_mark_style_props.preserved);
    canonical_preserved(&mut paragraph.literal_prefix_style_props.preserved);
    for carrier in [
        &mut paragraph.literal_prefix_leading_rpr,
        &mut paragraph.literal_prefix_trailing_rpr,
    ]
    .into_iter()
    .flatten()
    {
        canonical_preserved(&mut carrier.style_props.preserved);
        for run in &mut carrier.source_runs {
            canonical_preserved(&mut run.style_props.preserved);
        }
    }
    if let Some(change) = &mut paragraph.formatting_change {
        let previous = &mut change.previous;
        canonical_preserved(&mut previous.direct.preserved);
        if let crate::domain::DirectParagraphNumbering::Present(numbering) =
            &mut previous.direct.numbering
        {
            canonical_preserved(&mut numbering.preserved);
        }
        canonical_preserved(&mut previous.paragraph_mark.style_props.preserved);
    }

    for segment in &mut paragraph.segments {
        normalize_segment(segment, side);
    }
    // An untracked stretch left with nothing in it (it held only a proofing
    // mark or an empty run) is no stretch; untracked stretches that then
    // meet are one.
    let mut segments: Vec<TrackedSegment> = Vec::with_capacity(paragraph.segments.len());
    let mut joined = false;
    for segment in std::mem::take(&mut paragraph.segments) {
        let untracked = segment.status == crate::domain::TrackingStatus::Normal;
        if untracked && segment.inlines.is_empty() {
            continue;
        }
        if untracked
            && let Some(previous) = segments.last_mut()
            && previous.status == crate::domain::TrackingStatus::Normal
        {
            previous.inlines.extend(segment.inlines);
            joined = true;
            continue;
        }
        segments.push(segment);
    }
    if joined {
        // Text that now meets across the join is one stretch, as within a
        // segment.
        for segment in &mut segments {
            normalize_segment(segment, side);
        }
    }
    paragraph.segments = segments;
}

/// Spacing with what is unset written as the value it has: no space before
/// or after, single line spacing (ECMA-376 §17.3.1.33: 240 twentieths of a
/// point, `auto`).
fn spacing_with_defaults(spacing: Option<ParagraphSpacing>) -> ParagraphSpacing {
    let mut spacing = spacing.unwrap_or(ParagraphSpacing {
        before: None,
        after: None,
        before_lines: None,
        after_lines: None,
        before_autospacing: None,
        after_autospacing: None,
        line: None,
        line_rule: None,
    });
    spacing.before.get_or_insert(0);
    spacing.after.get_or_insert(0);
    if spacing.line.is_none() && spacing.line_rule.is_none() {
        spacing.line = Some(240);
        spacing.line_rule = Some(crate::domain::LineSpacingRule::Auto);
    }
    spacing
}

fn normalize_segment(segment: &mut TrackedSegment, side: Side<'_>) {
    let untracked = segment.status == crate::domain::TrackingStatus::Normal;
    let mut merged: Vec<InlineNode> = Vec::with_capacity(segment.inlines.len());
    for mut inline in std::mem::take(&mut segment.inlines) {
        match &mut inline {
            InlineNode::Text(text) => {
                text.source_run_attrs.clear();
                text.rpr_authored = RunRprAuthored::default();
                canonical_preserved(&mut text.style_props.preserved);
                canonical_change(&mut text.formatting_change);
                if let Some(InlineNode::Text(previous)) = merged.last_mut()
                    && same_formatting(previous, text)
                {
                    previous.text.push_str(&text.text);
                    continue;
                }
            }
            InlineNode::OpaqueInline(opaque) => {
                if let crate::domain::OpaqueKind::Hyperlink(link) = &mut opaque.kind {
                    normalize_hyperlink_runs(&mut link.runs, side);
                    // The link's other attributes in one order (XML 1.0 §3.1).
                    link.extra_attrs.sort();
                    // The address the link resolves to is held and compared;
                    // the id that named it is the package's bookkeeping.
                    if side.relationships.is_some() {
                        link.r_id = None;
                    }
                }
                opaque.source_run_attrs.clear();
                opaque.wrapper_rpr_authored = RunRprAuthored::default();
                canonical_preserved(&mut opaque.wrapper_style_props.preserved);
                canonical_change(&mut opaque.formatting_change);
                opaque.joins_following_text_run = false;
                // The hash of the held element, taken without the
                // revision-save ids inside it.
                if opaque.content_hash.is_some()
                    && let Some(raw) = &opaque.raw_xml
                {
                    opaque.content_hash = crate::import::opaque_content_hash_of(
                        &opaque.kind,
                        &canonical_held_element(raw, side),
                    );
                }
            }
            InlineNode::Decoration(decoration) => {
                // A proofing mark records where the spelling or grammar
                // checker stopped; it is not content and is not displayed.
                if decoration.kind == crate::domain::DecorationType::ProofError {
                    continue;
                }
                // A run with properties and no content (§17.3.2.25) shows
                // nothing; its properties format no text. Outside a tracked
                // change it is not content. Inside one it is what the
                // revision holds, and stays.
                if decoration.kind == crate::domain::DecorationType::EmptyRun && untracked {
                    continue;
                }
                if decoration.kind == crate::domain::DecorationType::Bookmark
                    && let Some(in_effect) = side.bookmarks
                    && decoration
                        .raw_xml
                        .as_deref()
                        .and_then(bookmark_marker)
                        .is_some()
                    && !in_effect.contains(&decoration.id.to_string())
                {
                    continue;
                }
                decoration.source_run_attrs.clear();
                decoration.joins_following_text_run = false;
                canonical_preserved(&mut decoration.wrapper_style_props.preserved);
                canonical_change(&mut decoration.formatting_change);
            }
            InlineNode::HardBreak(hard_break) => {
                hard_break.source_run_attrs.clear();
                hard_break.wrapper_rpr_authored = RunRprAuthored::default();
                canonical_preserved(&mut hard_break.wrapper_style_props.preserved);
                canonical_change(&mut hard_break.formatting_change);
                hard_break.joins_following_text_run = false;
            }
            InlineNode::CommentRangeStart { .. } | InlineNode::CommentRangeEnd { .. } => {}
        }
        merged.push(inline);
    }
    segment.inlines = merged;
}

/// Two stretches of text that differ only in where the run boundary falls.
fn same_formatting(a: &TextNode, b: &TextNode) -> bool {
    a.content_role() == b.content_role()
        && a.marks == b.marks
        && a.style_props == b.style_props
        && a.formatting_change == b.formatting_change
}

/// The runs of a hyperlink, without revision-save ids, one run per piece of
/// content, without runs that hold nothing, and with text runs of identical
/// properties joined.
///
/// A run is properties applied to a sequence of content (§17.3.2.25): one
/// run holding a tab, a field character and a field's result reads as the
/// three runs of the same properties holding one each. A held run is read
/// as those runs.
fn normalize_hyperlink_runs(runs: &mut Vec<crate::domain::HyperlinkRun>, side: Side<'_>) {
    let mut pieces: Vec<crate::domain::HyperlinkRun> = Vec::with_capacity(runs.len());
    for run in std::mem::take(runs) {
        match run
            .source_xml
            .as_deref()
            .and_then(|source| crate::word_xml::parse_raw_fragment(source).ok())
        {
            Some(mut element) => {
                // White space between elements is the writer's layout.
                strip_bookkeeping(&mut element);
                // A line feed in its text is the space Word reads.
                read_text_leaves(&mut element);
                for piece in run_pieces(&element) {
                    pieces.push(crate::word_ir::hyperlink_run(&piece, run.status.clone()));
                }
            }
            None => pieces.push(run),
        }
    }
    let mut merged: Vec<crate::domain::HyperlinkRun> = Vec::with_capacity(pieces.len());
    for mut run in pieces {
        // A run with properties and no content shows nothing.
        if run.source_xml.is_none()
            && run.text.is_empty()
            && run.status == crate::domain::TrackingStatus::Normal
        {
            continue;
        }
        run.source_run_attrs.clear();
        run.rpr_xml = run.rpr_xml.take().map(|raw| canonical_xml(&raw));
        for extra in &mut run.additional_rpr_xml {
            *extra = canonical_xml(extra);
        }
        if let Some(source) = run.source_xml.take() {
            run.source_xml = Some(canonical_held_element(&source, side));
        }
        if let Some(previous) = merged.last_mut()
            && previous.source_xml.is_none()
            && run.source_xml.is_none()
            && previous.rpr_xml == run.rpr_xml
            && previous.additional_rpr_xml == run.additional_rpr_xml
        {
            previous.text.push_str(&run.text);
            continue;
        }
        merged.push(run);
    }
    *runs = merged;
}

/// A run element as one run per content element, each with the run's
/// properties; a run with at most one content element as it is.
fn run_pieces(run: &xmltree::Element) -> Vec<xmltree::Element> {
    let (properties, content): (Vec<&xmltree::XMLNode>, Vec<&xmltree::XMLNode>) = run
        .children
        .iter()
        .filter(|child| matches!(child, xmltree::XMLNode::Element(_)))
        .partition(|child| {
            matches!(child, xmltree::XMLNode::Element(element)
                if element.name == "rPr"
                    && element.namespace.as_deref() == Some(WORDPROCESSINGML))
        });
    if content.len() <= 1 {
        return vec![run.clone()];
    }
    content
        .into_iter()
        .map(|item| {
            let mut piece = run.clone();
            piece.children = properties.iter().map(|p| (*p).clone()).collect();
            piece.children.push(item.clone());
            piece
        })
        .collect()
}

/// A held element without what records its editing history and layout of
/// the source text: revision-save ids (`w:rsid…`), paragraph and drawing
/// edit ids (`w14:paraId`, `w14:textId`, `wp14:anchorId`, `wp14:editId`),
/// and white space between elements; and with each element's attributes in
/// one order (XML 1.0 §3.1). Text inside a text-bearing element is kept as
/// it is. Where the side's relationships are known, a relationship
/// reference (`r:id`, `r:embed`, `r:link`, `r:pict`) is replaced by what it
/// points at.
///
/// Bytes that do not parse as a fragment are returned unchanged, and so
/// compare as they are.
fn canonical_held_element(xml: &[u8], side: Side<'_>) -> Vec<u8> {
    let Ok(mut element) = crate::word_xml::parse_raw_fragment(xml) else {
        return xml.to_vec();
    };
    strip_bookkeeping(&mut element);
    strip_identities(&mut element);
    canonical_prefixes(&mut element);
    sort_attributes(&mut element);
    read_text_leaves(&mut element);
    canonical_runs(&mut element);
    rewrite_references(&mut element, side, false);
    crate::word_xml::serialize_raw_fragment(&element)
}

/// Held content (a link's runs kept as written, an object's markup) with
/// the text of its `w:t` and `w:delText` read as Word reads it: a line feed
/// or carriage return there is the space Word reads, as in a paragraph's own
/// runs. A spelling with no information, so made equal under either
/// equality.
fn read_text_in_held_content(block: &mut BlockNode) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for inline in paragraph.segments.iter_mut().flat_map(|s| &mut s.inlines) {
                let InlineNode::OpaqueInline(opaque) = inline else {
                    continue;
                };
                if let crate::domain::OpaqueKind::Hyperlink(link) = &mut opaque.kind {
                    for run in &mut link.runs {
                        if let Some(source) = &run.source_xml
                            && let Ok(mut element) = crate::word_xml::parse_raw_fragment(source)
                            && read_text_leaves(&mut element)
                        {
                            // The run as the importer reads it once spelled
                            // as read: no held bytes when the fields rebuild it.
                            *run = crate::word_ir::hyperlink_run(&element, run.status.clone());
                        }
                    }
                }
                if let Some(raw) = &opaque.raw_xml
                    && let Ok(mut element) = crate::word_xml::parse_raw_fragment(raw)
                    && read_text_leaves(&mut element)
                {
                    let read = crate::word_xml::serialize_raw_fragment(&element);
                    if opaque.content_hash.is_some() {
                        opaque.content_hash =
                            crate::import::opaque_content_hash_of(&opaque.kind, &read);
                    }
                    opaque.raw_xml = Some(read);
                }
            }
        }
        BlockNode::Table(table) => {
            for block in table
                .rows
                .iter_mut()
                .flat_map(|row| &mut row.cells)
                .flat_map(|cell| &mut cell.blocks)
            {
                read_text_in_held_content(block);
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// The text of every `w:t` and `w:delText` inside a held element as Word
/// reads it: a line feed or carriage return written there is one space
/// ([`crate::word_xml::word_text_as_read`]), as the model reads a run.
/// Whether anything changed.
fn read_text_leaves(element: &mut xmltree::Element) -> bool {
    let leaf = is_w(element, "t") || is_w(element, "delText");
    let mut changed = false;
    for child in &mut element.children {
        match child {
            xmltree::XMLNode::Text(text) if leaf && text.contains(['\r', '\n']) => {
                *text = crate::word_xml::word_text_as_read(text);
                changed = true;
            }
            xmltree::XMLNode::Element(inner) => changed |= read_text_leaves(inner),
            _ => {}
        }
    }
    changed
}

fn is_w(element: &xmltree::Element, local: &str) -> bool {
    element.name == local && element.namespace.as_deref() == Some(WORDPROCESSINGML)
}

/// The runs inside a held element (a text box's paragraphs) read as the
/// model reads a paragraph's runs: without proofing marks, without runs
/// that hold nothing outside a tracked change, one run per piece of
/// content, and with adjacent runs of text in identical properties as one
/// run.
fn canonical_runs(element: &mut xmltree::Element) {
    /// The properties and text of a run that holds only text.
    fn text_run(node: &xmltree::XMLNode) -> Option<(Option<&xmltree::Element>, String)> {
        let xmltree::XMLNode::Element(run) = node else {
            return None;
        };
        if !is_w(run, "r") {
            return None;
        }
        let mut properties = None;
        let mut text = String::new();
        for child in &run.children {
            match child {
                xmltree::XMLNode::Element(e) if is_w(e, "rPr") && properties.is_none() => {
                    properties = Some(e);
                }
                xmltree::XMLNode::Element(e) if is_w(e, "t") => {
                    for node in &e.children {
                        match node {
                            xmltree::XMLNode::Text(t) => text.push_str(t),
                            _ => return None,
                        }
                    }
                }
                _ => return None,
            }
        }
        Some((properties, text))
    }
    join_math_runs(element);
    let tracked = ["ins", "del", "moveFrom", "moveTo"]
        .iter()
        .any(|local| is_w(element, local));
    // A run holding several pieces of content reads as runs of the same
    // properties holding one each (as a held hyperlink run is read).
    element.children = std::mem::take(&mut element.children)
        .into_iter()
        .flat_map(|child| match child {
            xmltree::XMLNode::Element(run) if is_w(&run, "r") => run_pieces(&run)
                .into_iter()
                .map(xmltree::XMLNode::Element)
                .collect(),
            other => vec![other],
        })
        .collect();
    element.children.retain(|child| match child {
        xmltree::XMLNode::Element(e) if is_w(e, "proofErr") => false,
        xmltree::XMLNode::Element(e) if is_w(e, "r") && !tracked => e
            .children
            .iter()
            .any(|c| !matches!(c, xmltree::XMLNode::Element(p) if is_w(p, "rPr"))),
        _ => true,
    });
    let mut joined: Vec<xmltree::XMLNode> = Vec::with_capacity(element.children.len());
    for child in std::mem::take(&mut element.children) {
        if let (Some(previous), Some((properties, text))) =
            (joined.last().and_then(text_run), text_run(&child))
            && previous.0 == properties
        {
            let mut combined = previous.1;
            combined.push_str(&text);
            let mut run = xmltree::Element::new("r");
            run.prefix = Some("w".into());
            run.namespace = Some(WORDPROCESSINGML.into());
            run.namespaces = None;
            if let Some(properties) = properties {
                run.children
                    .push(xmltree::XMLNode::Element(properties.clone()));
            }
            let mut t = xmltree::Element::new("t");
            t.prefix = Some("w".into());
            t.namespace = Some(WORDPROCESSINGML.into());
            if combined.starts_with(char::is_whitespace) || combined.ends_with(char::is_whitespace)
            {
                t.attributes.insert(
                    xmltree::AttributeName {
                        local_name: "space".into(),
                        namespace: Some("http://www.w3.org/XML/1998/namespace".into()),
                        prefix: Some("xml".into()),
                    },
                    "preserve".into(),
                );
            }
            t.children.push(xmltree::XMLNode::Text(combined));
            run.children.push(xmltree::XMLNode::Element(t));
            *joined.last_mut().expect("a previous run") = xmltree::XMLNode::Element(run);
            continue;
        }
        joined.push(child);
    }
    element.children = joined;
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            canonical_runs(child);
        }
    }
}

/// What a held element carries that names or encodes rather than says, for
/// appearance equality only: a content control's numeric id and the line
/// breaking of base64 graphic data. The fidelity comparison keeps both.
fn strip_identities(element: &mut xmltree::Element) {
    // A VML shape's own `id` names it for scripts and the editor; programs
    // rename ids that repeat. Nothing in the drawing refers to a shape by
    // it (a shape names its type through `type="#…"`, which is the id of a
    // `v:shapetype`, kept).
    if element.namespace.as_deref() == Some("urn:schemas-microsoft-com:vml")
        && element.name != "shapetype"
    {
        element
            .attributes
            .retain(|name, _| !(name.local_name == "id" && name.namespace.is_none()));
    }
    // A VML shape's `o:gfxdata` holds a binary in base64, in which white
    // space carries nothing (RFC 2045 §6.8). Writers break its lines with
    // an escaped or a literal line feed, which an XML parser reads as a line
    // feed or a space (XML 1.0 §3.3.3): the same data either way.
    for (name, value) in element.attributes.iter_mut() {
        if name.local_name == "gfxdata" && name.namespace.as_deref() == Some(VML_OFFICE) {
            value.retain(|character| !character.is_ascii_whitespace());
        }
    }
    // A content control's numeric id (`w:sdtPr/w:id`, §17.5.2.18) names the
    // control for the program that wrote it; a program renumbers ids that
    // collide. It is not displayed and does not bind the control's content
    // (a binding is `w:dataBinding`, kept).
    if element.name == "sdtPr" && element.namespace.as_deref() == Some(WORDPROCESSINGML) {
        element.children.retain(|child| {
            !matches!(child, xmltree::XMLNode::Element(id)
                if id.name == "id" && id.namespace.as_deref() == Some(WORDPROCESSINGML))
        });
    }
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            strip_identities(child);
        }
    }
}

fn strip_bookkeeping(element: &mut xmltree::Element) {
    element.attributes.retain(|name, _| {
        let local = name.local_name.as_str();
        !(local.starts_with("rsid") || matches!(local, "paraId" | "textId" | "anchorId" | "editId"))
    });
    let holds_elements = element
        .children
        .iter()
        .any(|child| matches!(child, xmltree::XMLNode::Element(_)));
    if holds_elements {
        element.children.retain(|child| match child {
            xmltree::XMLNode::Text(text) => !text.trim().is_empty(),
            _ => true,
        });
    }
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            strip_bookkeeping(child);
        }
    }
    // Each child of a property element sets a different property, so their
    // order carries nothing: one writer keeps the order it read, another
    // writes the schema's (§17.3.1.26, §17.3.2.28).
    if matches!(element.name.as_str(), "rPr" | "pPr")
        && element.namespace.as_deref() == Some(WORDPROCESSINGML)
    {
        element.children.sort_by(|a, b| {
            let key = |node: &xmltree::XMLNode| match node {
                xmltree::XMLNode::Element(child) => (
                    child.namespace.clone().unwrap_or_default(),
                    child.name.clone(),
                ),
                _ => (String::new(), String::new()),
            };
            key(a).cmp(&key(b))
        });
    }
    // An empty property element (`<w:rPr/>`, `<w:pPr/>`, once its
    // bookkeeping is gone) sets no property: it reads as no element.
    element.children.retain(|child| {
        !matches!(child, xmltree::XMLNode::Element(properties)
            if matches!(properties.name.as_str(), "rPr" | "pPr")
                && properties.namespace.as_deref() == Some(WORDPROCESSINGML)
                && properties.attributes.is_empty()
                && properties.children.is_empty())
    });
}

/// What a part is, for deciding that two relationships point at the same
/// thing: the SHA-256 of its bytes, or, for an XML part, of the XML without
/// bookkeeping and white space between elements, so a part another program
/// re-indented or re-declared is still the same part. A reference the XML
/// makes through one of the part's own relationships (any attribute in the
/// relationships namespace: a chart's `c:externalData r:id`, a control's
/// `ax:ocx r:id`) is read as what `relationships` says it points at, so a
/// part whose own relationships were renumbered is still the same part.
pub(crate) fn part_digest(
    bytes: &[u8],
    relationships: &std::collections::HashMap<String, String>,
) -> String {
    fn bind(
        element: &mut xmltree::Element,
        relationships: &std::collections::HashMap<String, String>,
    ) {
        for (name, value) in element.attributes.iter_mut() {
            if name.namespace.as_deref() == Some(RELATIONSHIPS)
                && let Some(target) = relationships.get(value.as_str())
            {
                *value = target.clone();
            }
        }
        for child in &mut element.children {
            if let xmltree::XMLNode::Element(child) = child {
                bind(child, relationships);
            }
        }
    }
    // A byte order mark announces the encoding (XML 1.0 §4.3.3); it is not
    // part of the document.
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match crate::word_xml::parse_raw_fragment(xml) {
        Ok(mut element) => {
            strip_bookkeeping(&mut element);
            bind(&mut element, relationships);
            // The canonical spelling: attribute order and unused namespace
            // declarations are a writer's choice, not content.
            crate::import::canonical_raw_hash(&crate::word_xml::serialize_raw_fragment(&element))
        }
        Err(_) => crate::import::sha256_hex(bytes),
    }
}

/// The list reference (`w:numPr`, §17.3.1.19) of each paragraph held in
/// `element`, in document order: `None` for a paragraph with none. A
/// reference without `w:ilvl` is to level 0. The previous properties of a
/// tracked paragraph change (`w:pPrChange`) are not the paragraph's own and
/// are not read.
fn held_references(element: &xmltree::Element, out: &mut Vec<Option<(u32, u32)>>) {
    fn child<'e>(element: &'e xmltree::Element, local: &str) -> Option<&'e xmltree::Element> {
        element.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(e) if is_w(e, local) => Some(e),
            _ => None,
        })
    }
    fn value(element: &xmltree::Element) -> Option<u32> {
        element
            .attributes
            .iter()
            .find(|(name, _)| {
                name.local_name == "val" && name.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .and_then(|(_, value)| value.parse().ok())
    }
    if is_w(element, "p") {
        let reference = child(element, "pPr")
            .and_then(|properties| child(properties, "numPr"))
            .and_then(|numbering| {
                let num_id = child(numbering, "numId").and_then(value)?;
                let level = child(numbering, "ilvl").and_then(value).unwrap_or(0);
                Some((num_id, level))
            });
        out.push(reference);
    }
    for node in &element.children {
        if let xmltree::XMLNode::Element(e) = node {
            held_references(e, out);
        }
    }
}

/// Differences in how the paragraphs of two blocks are numbered: a pair of
/// paragraphs (taken in order, tables included) whose list references
/// resolve to different level definitions. The model's own comparison sees
/// the reference (`w:numId`, `w:ilvl`), not the definition behind it, so a
/// marker or number format changed in the numbering part alone would pass.
/// Empty when either side's package is not known.
pub(crate) fn numbering_differences(
    before: &TrackedBlock,
    after: &TrackedBlock,
    before_side: Side<'_>,
    after_side: Side<'_>,
) -> Vec<crate::roundtrip_compare::Difference> {
    let (Some(before_facts), Some(after_facts)) = (before_side.parts, after_side.parts) else {
        return Vec::new();
    };
    fn references(block: &BlockNode, out: &mut Vec<Option<(u32, u32)>>) {
        match block {
            BlockNode::Paragraph(paragraph) => {
                out.push(
                    paragraph
                        .numbering
                        .as_ref()
                        .map(|numbering| (numbering.num_id, numbering.ilvl)),
                );
                // A text box's paragraphs are held in their drawing's XML;
                // their list references are read from it.
                for inline in paragraph.segments.iter().flat_map(|s| &s.inlines) {
                    if let InlineNode::OpaqueInline(opaque) = inline
                        && let Some(raw) = &opaque.raw_xml
                        && let Ok(element) = crate::word_xml::parse_raw_fragment(raw)
                    {
                        held_references(&element, out);
                    }
                }
            }
            BlockNode::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        for block in &cell.blocks {
                            references(block, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    references(&before.block, &mut left);
    references(&after.block, &mut right);
    if left.len() != right.len() {
        // The blocks differ in structure; the block comparison reports it.
        return Vec::new();
    }
    let mut differences = Vec::new();
    for (index, pair) in left.iter().zip(&right).enumerate() {
        let (Some(before_reference), Some(after_reference)) = pair else {
            continue;
        };
        let before_level = before_facts.numbering_levels.get(before_reference);
        let after_level = after_facts.numbering_levels.get(after_reference);
        if before_level != after_level {
            differences.push(crate::roundtrip_compare::Difference {
                path: format!("block.paragraphs[{index}].numbering.level_definition"),
                left: format!("{before_reference:?} -> {before_level:?}"),
                right: format!("{after_reference:?} -> {after_level:?}"),
            });
        }
    }
    differences
}

/// A digest of one list level definition (a `w:lvl` element, with the start
/// override that applies to it, if any), without bookkeeping.
pub(crate) fn level_digest(level: &xmltree::Element, start_override: Option<&str>) -> String {
    let mut level = level.clone();
    strip_bookkeeping(&mut level);
    // `w:tplc` is a template code Word assigns; it does not affect the list.
    level.attributes.retain(|name, _| name.local_name != "tplc");
    // The canonical spelling: attribute order and unused namespace
    // declarations are a writer's choice, not content.
    let level = crate::import::canonical_raw_hash(&crate::word_xml::serialize_raw_fragment(&level));
    match start_override {
        Some(start) => format!("{level}|startOverride={start}"),
        None => level,
    }
}

/// Held XML in one spelling: without bookkeeping, and with each element's
/// attributes in one order (their order is not significant, XML 1.0 §3.1).
/// Bytes that do not parse as a fragment are returned unchanged.
fn canonical_xml(raw: &[u8]) -> Vec<u8> {
    let Ok(mut element) = crate::word_xml::parse_raw_fragment(raw) else {
        return raw.to_vec();
    };
    strip_bookkeeping(&mut element);
    sort_attributes(&mut element);
    crate::word_xml::serialize_raw_fragment(&element)
}

/// Each element's attributes in one order: their order is not significant
/// (XML 1.0 §3.1).
fn sort_attributes(element: &mut xmltree::Element) {
    element.attributes.sort_by(|a, _, b, _| {
        (a.namespace.as_deref(), a.local_name.as_str())
            .cmp(&(b.namespace.as_deref(), b.local_name.as_str()))
    });
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            sort_attributes(child);
        }
    }
}

/// A tracked run-formatting change with the previous properties' held XML in
/// one spelling.
fn canonical_change(change: &mut Option<crate::domain::FormattingChange>) {
    if let Some(change) = change {
        canonical_preserved(&mut change.previous_style_props.preserved);
    }
}

/// The properties a model holds as XML because it has no typed slot for
/// them, each in one spelling.
fn canonical_preserved(preserved: &mut [crate::domain::PreservedProp]) {
    for property in preserved {
        property.raw_xml =
            String::from_utf8_lossy(&canonical_xml(property.raw_xml.as_bytes())).into_owned();
    }
}

/// Settings (`w:settings`, §17.15.1) that record how the document was
/// edited, viewed or saved, not how it reads or looks. Each is the writing
/// program's state, not the document's:
///
/// - editing history and checker state: `w:rsids` (revision-save ids),
///   `w:proofState`, `w:activeWritingStyle`, `w:hideSpellingErrors`,
///   `w:hideGrammaticalErrors`;
/// - identity: `w14:docId`, `w15:docId`, `w15:chartTrackingRefBased`,
///   `w:attachedTemplate`, `w:docVars` (shown only by a field, through its
///   cached result, which is compared where it stands);
/// - view: `w:zoom`, `w:view`, `w:revisionView`,
///   `w:doNotDisplayPageBoundaries`, the drawing grid
///   (`w:drawingGrid…`, `w:display…DrawingGridEvery`,
///   `w:doNotUseMarginsForDrawingGridOrigin`), `w:stylePaneFormatFilter`,
///   `w:stylePaneSortMethod`, `w:uiCompat97To2003`, `w:showXMLTags`,
///   `w:readModeInkLockDown`;
/// - editing behaviour: `w:trackRevisions`, `w:doNotTrackMoves`,
///   `w:doNotTrackFormatting`, `w:documentProtection`, `w:writeProtection`,
///   `w:formsDesign`, `w:autoFormatOverride`, `w:styleLockTheme`,
///   `w:styleLockQFSet`, `w:clickAndTypeStyle`, `w:defaultTableStyle`,
///   `w:shapeDefaults`, `w:hdrShapeDefaults`, `w:captions`, `w:smartTagType`,
///   `w:doNotEmbedSmartTags`, `w:attachedSchema`, `w:mailMerge`;
/// - saving: `w:removePersonalInformation`, `w:removeDateAndTime`,
///   `w:embedTrueTypeFonts`, `w:embedSystemFonts`, `w:saveSubsetFonts`,
///   `w:saveFormsData`, `w:savePreviewPicture`, `w:doNotAutoCompressPictures`,
///   `w14:defaultImageDpi`, `w14:discardImageEditingData`,
///   `w:doNotIncludeSubdocsInStats`, and the custom XML save options
///   (`w:doNotValidateAgainstSchema`, `w:saveInvalidXml`,
///   `w:ignoreMixedContent`, `w:alwaysShowPlaceholderText`,
///   `w:doNotDemarcateInvalidXml`, `w:saveXmlDataOnly`,
///   `w:useXSLTWhenSaving`, `w:saveThroughXslt`,
///   `w:alwaysMergeEmptyNamespace`).
const BOOKKEEPING_SETTINGS: &[&str] = &[
    "rsids",
    "proofState",
    "activeWritingStyle",
    "hideSpellingErrors",
    "hideGrammaticalErrors",
    "docId",
    "chartTrackingRefBased",
    "attachedTemplate",
    "docVars",
    "zoom",
    "view",
    "revisionView",
    "doNotDisplayPageBoundaries",
    "drawingGridHorizontalSpacing",
    "drawingGridVerticalSpacing",
    "drawingGridHorizontalOrigin",
    "drawingGridVerticalOrigin",
    "displayHorizontalDrawingGridEvery",
    "displayVerticalDrawingGridEvery",
    "doNotUseMarginsForDrawingGridOrigin",
    "stylePaneFormatFilter",
    "stylePaneSortMethod",
    "uiCompat97To2003",
    "showXMLTags",
    "readModeInkLockDown",
    "trackRevisions",
    "doNotTrackMoves",
    "doNotTrackFormatting",
    "documentProtection",
    "writeProtection",
    "formsDesign",
    "autoFormatOverride",
    "styleLockTheme",
    "styleLockQFSet",
    "clickAndTypeStyle",
    "defaultTableStyle",
    "shapeDefaults",
    "hdrShapeDefaults",
    "captions",
    "smartTagType",
    "doNotEmbedSmartTags",
    "attachedSchema",
    "mailMerge",
    "removePersonalInformation",
    "removeDateAndTime",
    "embedTrueTypeFonts",
    "embedSystemFonts",
    "saveSubsetFonts",
    "saveFormsData",
    "savePreviewPicture",
    "doNotAutoCompressPictures",
    "defaultImageDpi",
    "discardImageEditingData",
    "doNotIncludeSubdocsInStats",
    "doNotValidateAgainstSchema",
    "saveInvalidXml",
    "ignoreMixedContent",
    "alwaysShowPlaceholderText",
    "doNotDemarcateInvalidXml",
    "saveXmlDataOnly",
    "useXSLTWhenSaving",
    "saveThroughXslt",
    "alwaysMergeEmptyNamespace",
];

/// Settings whose value is on or off (`CT_OnOff`, §17.17.4) and off when
/// the element is absent: written off, they are not written.
const ON_OFF_SETTINGS: &[&str] = &[
    "mirrorMargins",
    "alignBordersAndEdges",
    "bordersDoNotSurroundHeader",
    "bordersDoNotSurroundFooter",
    "gutterAtTop",
    "displayBackgroundShape",
    "printPostScriptOverText",
    "printFractionalCharacterWidth",
    "printFormsData",
    "linkStyles",
    "autoHyphenation",
    "doNotHyphenateCaps",
    "showEnvelope",
    "evenAndOddHeaders",
    "bookFoldRevPrinting",
    "bookFoldPrinting",
    "doNotShadeFormData",
    "noPunctuationKerning",
    "printTwoOnOne",
    "strictFirstAndLastChars",
    "updateFields",
];

/// The settings of a settings part that bear on how the document reads or
/// looks: every child of `w:settings` but the [`BOOKKEEPING_SETTINGS`], in
/// one spelling. Attribute order, bookkeeping attributes and the spelling
/// of an on/off value (`true`, `on`, `1`, a bare element) are not content;
/// a setting written as its default reads as the setting left out: an
/// on/off setting written off, `w:characterSpacingControl` written
/// `doNotCompress` (§17.15.1.18), `w:defaultTabStop` written 720
/// (§17.15.1.25), and `w:clrSchemeMapping` written as the identity mapping
/// (§17.15.1.20).
pub(crate) fn settings_in_effect(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("settings part does not parse: {error}"))?;
    let mut settings: Vec<(String, String)> = Vec::new();
    for node in &root.children {
        let xmltree::XMLNode::Element(element) = node else {
            continue;
        };
        let local = element.name.as_str();
        if BOOKKEEPING_SETTINGS.contains(&local) {
            continue;
        }
        let mut element = element.clone();
        strip_bookkeeping(&mut element);
        sort_attributes(&mut element);
        canonical_on_off(&mut element);
        if element.children.is_empty() && ON_OFF_SETTINGS.contains(&local) {
            match element_value(&element).as_deref() {
                Some("0") => continue,
                Some("1") => element.attributes.clear(),
                _ => {}
            }
        }
        if is_w(&element, "characterSpacingControl")
            && element_value(&element).as_deref() == Some("doNotCompress")
        {
            continue;
        }
        // §17.15.1.25: without the setting, default tab stops fall every
        // 720 twentieths of a point.
        if is_w(&element, "defaultTabStop") && element_value(&element).as_deref() == Some("720") {
            continue;
        }
        if is_w(&element, "clrSchemeMapping") && is_identity_color_mapping(&element) {
            continue;
        }
        if is_w(&element, "compat") {
            canonical_compat(&mut element);
            if element.children.is_empty() {
                continue;
            }
        }
        if is_w(&element, "themeFontLang") {
            // `w:val` names the language of Latin text; the theme's Latin
            // fonts (`a:latin`) have no per-language variants, so it
            // selects nothing. The East Asian and complex script languages
            // do select a theme font (`a:font w:script`) and stay.
            element.attributes.retain(|name, _| {
                !(name.local_name == "val" && name.namespace.as_deref() == Some(WORDPROCESSINGML))
            });
            if element.attributes.is_empty() {
                continue;
            }
        }
        let name = match element.namespace.as_deref() {
            Some(WORDPROCESSINGML) | None => local.to_string(),
            Some(namespace) => format!("{{{namespace}}}{local}"),
        };
        let spelling = String::from_utf8_lossy(&crate::word_xml::serialize_raw_fragment(&element))
            .into_owned();
        settings.push((name, spelling));
    }
    settings.sort();
    Ok(settings)
}

/// The `w:val` of a settings element.
fn element_value(element: &xmltree::Element) -> Option<String> {
    element
        .attributes
        .iter()
        .find(|(name, _)| {
            name.local_name == "val" && name.namespace.as_deref() == Some(WORDPROCESSINGML)
        })
        .map(|(_, value)| value.clone())
}

/// Each `w:val` written `true`/`on` as `1` and `false`/`off` as `0`
/// (`ST_OnOff`, §22.9.2.7), throughout `element`.
fn canonical_on_off(element: &mut xmltree::Element) {
    for (name, value) in element.attributes.iter_mut() {
        if name.local_name == "val" && name.namespace.as_deref() == Some(WORDPROCESSINGML) {
            match value.as_str() {
                "true" | "on" => *value = "1".to_string(),
                "false" | "off" => *value = "0".to_string(),
                _ => {}
            }
        }
    }
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            canonical_on_off(child);
        }
    }
}

/// Whether a `w:clrSchemeMapping` maps every theme colour to itself, which
/// is what an absent mapping does.
fn is_identity_color_mapping(element: &xmltree::Element) -> bool {
    const IDENTITY: &[(&str, &str)] = &[
        ("bg1", "light1"),
        ("t1", "dark1"),
        ("bg2", "light2"),
        ("t2", "dark2"),
        ("accent1", "accent1"),
        ("accent2", "accent2"),
        ("accent3", "accent3"),
        ("accent4", "accent4"),
        ("accent5", "accent5"),
        ("accent6", "accent6"),
        ("hyperlink", "hyperlink"),
        ("followedHyperlink", "followedHyperlink"),
    ];
    element.children.is_empty()
        && element.attributes.len() == IDENTITY.len()
        && IDENTITY.iter().all(|(slot, colour)| {
            element.attributes.iter().any(|(name, value)| {
                name.local_name == *slot
                    && name.namespace.as_deref() == Some(WORDPROCESSINGML)
                    && value == colour
            })
        })
}

/// Per theme colour a run can name (`ST_ThemeColor`, §17.18.97), the colour
/// the theme part's colour scheme (`a:clrScheme`, DrawingML §20.1.6.2)
/// gives it, as the scheme entry's canonical XML. `text1`/`background1`/
/// `text2`/`background2` read `dk1`/`lt1`/`dk2`/`lt2`, the mapping a
/// document uses unless its settings map otherwise (a mapping is compared
/// with the settings).
pub(crate) fn theme_colors(
    bytes: &[u8],
) -> Result<std::collections::HashMap<String, String>, String> {
    const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const SLOTS: &[(&str, &[&str])] = &[
        ("dk1", &["dark1", "text1"]),
        ("lt1", &["light1", "background1"]),
        ("dk2", &["dark2", "text2"]),
        ("lt2", &["light2", "background2"]),
        ("accent1", &["accent1"]),
        ("accent2", &["accent2"]),
        ("accent3", &["accent3"]),
        ("accent4", &["accent4"]),
        ("accent5", &["accent5"]),
        ("accent6", &["accent6"]),
        ("hlink", &["hyperlink"]),
        ("folHlink", &["followedHyperlink"]),
    ];
    fn find<'e>(element: &'e xmltree::Element, local: &str) -> Option<&'e xmltree::Element> {
        if element.name == local && element.namespace.as_deref() == Some(DRAWINGML) {
            return Some(element);
        }
        element.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(child) => find(child, local),
            _ => None,
        })
    }
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("theme part does not parse: {error}"))?;
    let mut colors = std::collections::HashMap::new();
    let Some(scheme) = find(&root, "clrScheme") else {
        return Ok(colors);
    };
    for (slot, names) in SLOTS {
        let Some(entry) = scheme.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(e)
                if e.name == *slot && e.namespace.as_deref() == Some(DRAWINGML) =>
            {
                Some(e)
            }
            _ => None,
        }) else {
            continue;
        };
        let mut entry = entry.clone();
        strip_bookkeeping(&mut entry);
        sort_attributes(&mut entry);
        entry.name = "colour".to_string();
        let value =
            String::from_utf8_lossy(&crate::word_xml::serialize_raw_fragment(&entry)).into_owned();
        for name in *names {
            colors.insert((*name).to_string(), value.clone());
        }
    }
    Ok(colors)
}

/// Differences in run formatting that reaches a block's text through the
/// styles part, where the block itself does not hold the resolved value:
///
/// - a hyperlink's runs hold only their own properties (`w:rPr`); what the
///   character style, the paragraph style and the document defaults add is
///   resolved here (§17.7.2) and compared, run by run, with runs of equal
///   resolved formatting joined;
/// - a paragraph style linked to a character style (§17.7.4.6): the model
///   resolves the paragraph's text through the pair merged, so a property
///   both styles set reads from the character style. What the paragraph
///   style's own chain sets is compared as well: changed there, it may be
///   what the paragraph shows.
///
/// Paragraphs are taken in order, tables included; empty when either side's
/// styles are not known or the blocks differ in structure.
pub(crate) fn style_differences(
    before: &TrackedBlock,
    after: &TrackedBlock,
    before_side: Side<'_>,
    after_side: Side<'_>,
) -> Vec<crate::roundtrip_compare::Difference> {
    let (Some(before_styles), Some(after_styles)) = (before_side.styles, after_side.styles) else {
        return Vec::new();
    };
    // The same styles part on both sides gives the same text the same
    // formatting; what the blocks hold themselves is compared with them.
    if let (Some(before_facts), Some(after_facts)) = (before_side.parts, after_side.parts)
        && before_facts.styles_digest.is_some()
        && before_facts.styles_digest == after_facts.styles_digest
        && before_facts.theme_digest == after_facts.theme_digest
        && before_facts.numbering_levels == after_facts.numbering_levels
    {
        return Vec::new();
    }
    fn paragraphs<'b>(block: &'b BlockNode, out: &mut Vec<&'b ParagraphNode>) {
        match block {
            BlockNode::Paragraph(paragraph) => out.push(paragraph),
            BlockNode::Table(table) => {
                for block in table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .flat_map(|cell| &cell.blocks)
                {
                    paragraphs(block, out);
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    paragraphs(&before.block, &mut left);
    paragraphs(&after.block, &mut right);
    if left.len() != right.len() {
        return Vec::new();
    }
    let mut differences = Vec::new();
    for (index, (a, b)) in left.iter().zip(&right).enumerate() {
        let own = |paragraph: &ParagraphNode,
                   styles: &crate::styles::StyleDefinitions,
                   side: Side<'_>| {
            paragraph_style_id(paragraph, styles)
                .and_then(|id| styles.linked_paragraph_style_own_marks(id))
                .map(|marks| marks_key(marks, side))
        };
        let (own_a, own_b) = (
            own(a, before_styles, before_side),
            own(b, after_styles, after_side),
        );
        if own_a != own_b {
            differences.push(crate::roundtrip_compare::Difference {
                path: format!("block.paragraphs[{index}].linked_paragraph_style.run_properties"),
                left: format!("{own_a:?}"),
                right: format!("{own_b:?}"),
            });
        }
        // Numbering a paragraph takes from its style, where it has none of
        // its own: the model reads a style's list either from the style's
        // `w:numPr` or from a list level that names the style (§17.9.23),
        // and either may be what a reader sees, so the style's own is
        // compared too.
        if !a.has_direct_numbering && !b.has_direct_numbering {
            let style_list = |paragraph: &ParagraphNode,
                              styles: &crate::styles::StyleDefinitions,
                              side: Side<'_>| {
                match styles.resolve_effective_num_props(
                    paragraph_style_id(paragraph, styles),
                    &crate::word_ir::DirectNumPr::Absent,
                ) {
                    crate::word_ir::DirectNumPr::Active(list) => format!(
                        "list level {:?}",
                        side.parts.and_then(|facts| facts
                            .numbering_levels
                            .get(&(list.num_id, list.ilvl)))
                    ),
                    other => format!("{other:?}"),
                }
            };
            let (list_a, list_b) = (
                style_list(a, before_styles, before_side),
                style_list(b, after_styles, after_side),
            );
            if list_a != list_b {
                differences.push(crate::roundtrip_compare::Difference {
                    path: format!("block.paragraphs[{index}].paragraph_style.numbering"),
                    left: list_a,
                    right: list_b,
                });
            }
        }
        // The paragraph properties the paragraph style's chain writes.
        if let (Some(before_facts), Some(after_facts)) = (before_side.parts, after_side.parts) {
            let properties = |paragraph: &ParagraphNode,
                              styles: &crate::styles::StyleDefinitions,
                              facts: &PackageFacts| {
                paragraph_style_id(paragraph, styles)
                    .and_then(|id| facts.paragraph_styles.get(id))
                    .cloned()
            };
            let (style_a, style_b) = (
                properties(a, before_styles, before_facts),
                properties(b, after_styles, after_facts),
            );
            if style_a != style_b {
                differences.push(crate::roundtrip_compare::Difference {
                    path: format!("block.paragraphs[{index}].paragraph_style.properties"),
                    left: format!("{style_a:?}"),
                    right: format!("{style_b:?}"),
                });
            }
        }
        // Tab stops place text only where the paragraph has a tab: a tab
        // character, the tab after a list number, or one around a typed
        // label the model holds apart from the text.
        let uses_tabs = |paragraph: &ParagraphNode| {
            paragraph.numbering.is_some()
                || paragraph.literal_prefix_has_trailing_tab
                || paragraph.literal_prefix_leading_tab_count > 0
                || paragraph
                    .segments
                    .iter()
                    .flat_map(|s| &s.inlines)
                    .any(|inline| match inline {
                        InlineNode::Text(text) => text.text.contains('\t'),
                        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                            crate::domain::OpaqueKind::Hyperlink(link) => {
                                link.runs.iter().any(|run| run.text.contains('\t'))
                            }
                            _ => opaque.raw_xml.as_deref().is_some_and(holds_tab),
                        },
                        _ => false,
                    })
        };
        if uses_tabs(a) || uses_tabs(b) {
            // The stops the paragraph style's chain and the paragraph's own
            // `w:tabs` give it (leaders included).
            let stops = |paragraph: &ParagraphNode, styles: &crate::styles::StyleDefinitions| {
                styles.resolve_effective_tabs(
                    paragraph_style_id(paragraph, styles),
                    Some(paragraph.tab_stops.as_slice()),
                )
            };
            let (stops_a, stops_b) = (stops(a, before_styles), stops(b, after_styles));
            if stops_a != stops_b {
                differences.push(crate::roundtrip_compare::Difference {
                    path: format!("block.paragraphs[{index}].effective_tab_stops"),
                    left: format!("{stops_a:?}"),
                    right: format!("{stops_b:?}"),
                });
            }
        }
        let (held_a, held_b) = (
            held_run_formatting(a, before_styles, before_side),
            held_run_formatting(b, after_styles, after_side),
        );
        if held_a != held_b {
            differences.push(crate::roundtrip_compare::Difference {
                path: format!("block.paragraphs[{index}].held_runs.resolved_formatting"),
                left: format!("{held_a:?}"),
                right: format!("{held_b:?}"),
            });
        }
        let (links_a, links_b) = (
            hyperlink_formatting(a, before_styles, before_side),
            hyperlink_formatting(b, after_styles, after_side),
        );
        if links_a != links_b {
            differences.push(crate::roundtrip_compare::Difference {
                path: format!("block.paragraphs[{index}].hyperlink_runs.resolved_formatting"),
                left: format!("{links_a:?}"),
                right: format!("{links_b:?}"),
            });
        }
    }
    differences
}

/// The paragraph style a paragraph resolves through: its own when the
/// styles part defines it, else the default paragraph style (§17.3.1.27).
fn paragraph_style_id<'a>(
    paragraph: &'a ParagraphNode,
    styles: &'a crate::styles::StyleDefinitions,
) -> Option<&'a str> {
    match paragraph.style_id.as_deref() {
        Some(id) if styles.defines_paragraph_style(id) => Some(id),
        _ => styles.default_para_style_id(),
    }
}

/// Resolved run formatting in a form two sides compare by: the theme colour
/// it names joined to the side's colour for it.
fn marks_key(marks: &crate::word_ir::TextMarks, side: Side<'_>) -> String {
    let mut marks = marks.clone();
    if let (Some(reference), Some(facts)) = (&mut marks.color_theme, side.parts) {
        let colour = facts
            .theme_colors
            .get(reference.theme_color.as_ref())
            .map(String::as_str)
            .unwrap_or("?");
        reference.theme_color = format!("{}={colour}", reference.theme_color).into();
    }
    // Where the run properties were written, not what they resolve to.
    marks.rpr_after_content = false;
    marks.authored_rfonts = Default::default();
    format!("{marks:?}")
}

/// The text of a paragraph's hyperlinks with the formatting each run
/// resolves to through the styles part, runs of equal formatting joined.
fn hyperlink_formatting(
    paragraph: &ParagraphNode,
    styles: &crate::styles::StyleDefinitions,
    side: Side<'_>,
) -> Vec<(String, String)> {
    let paragraph_style = paragraph_style_id(paragraph, styles);
    let mut runs: Vec<(String, String)> = Vec::new();
    for inline in paragraph.segments.iter().flat_map(|s| &s.inlines) {
        let InlineNode::OpaqueInline(opaque) = inline else {
            continue;
        };
        let crate::domain::OpaqueKind::Hyperlink(link) = &opaque.kind else {
            continue;
        };
        for run in &link.runs {
            if run.text.is_empty() {
                continue;
            }
            let key = match run
                .rpr_xml
                .as_deref()
                .map(crate::word_xml::parse_raw_fragment)
            {
                None => marks_key(
                    &styles.resolve(&Default::default(), None, paragraph_style),
                    side,
                ),
                Some(Ok(properties)) => {
                    let character_style = properties.children.iter().find_map(|node| match node {
                        xmltree::XMLNode::Element(e) if is_w(e, "rStyle") => element_value(e),
                        _ => None,
                    });
                    match crate::styles::parse_rpr_marks(&properties) {
                        Ok(direct) => marks_key(
                            &styles.resolve(&direct, character_style.as_deref(), paragraph_style),
                            side,
                        ),
                        Err(error) => format!("run properties do not read: {error}"),
                    }
                }
                Some(Err(error)) => format!("run properties do not parse: {error}"),
            };
            match runs.last_mut() {
                Some((previous, text)) if *previous == key => text.push_str(&run.text),
                _ => runs.push((key, run.text.clone())),
            }
        }
    }
    runs
}

/// `w:compat` (§17.15.1.21) without what it writes as a default: an option
/// written off (each compatibility option is on/off, off when absent), and
/// `compatibilityMode` 12, which a document without the setting has
/// (MS-DOCX §2.3.5).
fn canonical_compat(compat: &mut xmltree::Element) {
    compat.children.retain(|node| {
        let xmltree::XMLNode::Element(option) = node else {
            return false;
        };
        let name = option
            .attributes
            .iter()
            .find(|(attribute, _)| {
                attribute.local_name == "name"
                    && attribute.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .map(|(_, value)| value.as_str());
        let value = element_value(option);
        if is_w(option, "compatSetting") && name == Some("compatibilityMode") {
            return value.as_deref() != Some("12");
        }
        value.as_deref() != Some("0")
    });
}

const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

/// What the theme part (DrawingML §20.1.6) gives a document, entry by entry:
/// each colour of the colour scheme under every name a document can call it
/// by (`w:themeColor`/`w:themeFill`, §17.18.97, and `a:schemeClr`,
/// §20.1.10.54), each font of the font scheme under its names (`w:asciiTheme`
/// and the like, §17.18.96, and `+mn-lt` and the like in a typeface), and
/// the format scheme (`a:fmtScheme`, the fills, lines and effects a shape's
/// style refers to by index).
fn theme_entries(bytes: &[u8]) -> Result<std::collections::HashMap<String, String>, String> {
    fn child<'e>(element: &'e xmltree::Element, local: &str) -> Option<&'e xmltree::Element> {
        element.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(e)
                if e.name == local && e.namespace.as_deref() == Some(DRAWINGML) =>
            {
                Some(e)
            }
            _ => None,
        })
    }
    fn find<'e>(element: &'e xmltree::Element, local: &str) -> Option<&'e xmltree::Element> {
        if element.name == local && element.namespace.as_deref() == Some(DRAWINGML) {
            return Some(element);
        }
        element.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(e) => find(e, local),
            _ => None,
        })
    }
    fn spelling(element: &xmltree::Element) -> String {
        let mut element = element.clone();
        strip_bookkeeping(&mut element);
        sort_attributes(&mut element);
        String::from_utf8_lossy(&crate::word_xml::serialize_raw_fragment(&element)).into_owned()
    }
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("theme part does not parse: {error}"))?;
    let mut entries = std::collections::HashMap::new();
    if let Some(scheme) = find(&root, "clrScheme") {
        const COLOURS: &[(&str, &[&str])] = &[
            ("dk1", &["dark1", "text1", "tx1", "dk1"]),
            ("lt1", &["light1", "background1", "bg1", "lt1"]),
            ("dk2", &["dark2", "text2", "tx2", "dk2"]),
            ("lt2", &["light2", "background2", "bg2", "lt2"]),
            ("accent1", &["accent1"]),
            ("accent2", &["accent2"]),
            ("accent3", &["accent3"]),
            ("accent4", &["accent4"]),
            ("accent5", &["accent5"]),
            ("accent6", &["accent6"]),
            ("hlink", &["hyperlink", "hlink"]),
            ("folHlink", &["followedHyperlink", "folHlink"]),
        ];
        for (slot, names) in COLOURS {
            if let Some(entry) = child(scheme, slot) {
                let mut entry = entry.clone();
                entry.name = "colour".to_string();
                let value = spelling(&entry);
                for name in *names {
                    entries.insert(format!("colour:{name}"), value.clone());
                }
            }
        }
    }
    if let Some(scheme) = find(&root, "fontScheme") {
        for (group, short) in [("majorFont", "major"), ("minorFont", "minor")] {
            let Some(fonts) = child(scheme, group) else {
                continue;
            };
            let mut group_spelling = fonts.clone();
            group_spelling.name = "fonts".to_string();
            let all = spelling(&group_spelling);
            for (script, names, plus) in [
                ("latin", &["HAnsi", "Ascii"][..], "lt"),
                ("ea", &["EastAsia"][..], "ea"),
                ("cs", &["Bidi"][..], "cs"),
            ] {
                // A script's font is its typeface, or for a typeface left
                // empty, the per-script fonts (`a:font`) the group lists.
                let value = match child(fonts, script) {
                    Some(font) => format!("{}|{all}", spelling(font)),
                    None => all.clone(),
                };
                for name in names {
                    entries.insert(format!("font:{short}{name}"), value.clone());
                }
                let prefix = if short == "major" { "mj" } else { "mn" };
                entries.insert(format!("font:+{prefix}-{plus}"), value.clone());
            }
            entries.insert(format!("font:{short}"), all);
        }
    }
    if let Some(format) = find(&root, "fmtScheme") {
        entries.insert("format".to_string(), spelling(format));
    }
    Ok(entries)
}

/// The theme entries a document uses: names that `parts` (the XML of the
/// parts whose content is shown, styles in use only) refer to, joined to
/// what the theme gives them; `?` where the theme gives nothing.
pub(crate) fn theme_in_use(
    theme: Option<&[u8]>,
    parts: &[xmltree::Element],
) -> Result<Vec<(String, String)>, String> {
    fn collect(element: &xmltree::Element, used: &mut std::collections::BTreeSet<String>) {
        let a = element.namespace.as_deref() == Some(DRAWINGML);
        for (name, value) in &element.attributes {
            let attribute_w = name.namespace.as_deref() == Some(WORDPROCESSINGML);
            match name.local_name.as_str() {
                "themeColor" | "themeFill" if attribute_w && value != "none" => {
                    used.insert(format!("colour:{value}"));
                }
                "asciiTheme" | "hAnsiTheme" | "eastAsiaTheme" | "cstheme" if attribute_w => {
                    used.insert(format!("font:{value}"));
                }
                "val" if a && element.name == "schemeClr" && value != "phClr" => {
                    used.insert(format!("colour:{value}"));
                }
                "typeface" if a && value.starts_with('+') => {
                    used.insert(format!("font:{value}"));
                }
                "idx" if a && element.name == "fontRef" => {
                    used.insert(format!("font:{value}"));
                }
                _ => {}
            }
        }
        if a && matches!(element.name.as_str(), "fillRef" | "lnRef" | "effectRef") {
            used.insert("format".to_string());
        }
        for node in &element.children {
            if let xmltree::XMLNode::Element(child) = node {
                collect(child, used);
            }
        }
    }
    let mut used = std::collections::BTreeSet::new();
    for part in parts {
        collect(part, &mut used);
    }
    if used.is_empty() {
        return Ok(Vec::new());
    }
    let entries = match theme {
        Some(bytes) => theme_entries(bytes)?,
        None => std::collections::HashMap::new(),
    };
    Ok(used
        .into_iter()
        .map(|name| {
            let value = entries
                .get(&name)
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            (name, value)
        })
        .collect())
}

/// The styles a document's content uses (§17.7): the styles its content
/// names (`w:pStyle`, `w:rStyle`, `w:tblStyle`, and `w:numStyleLink`/
/// `w:styleLink` of numbering), the default style of each type, and every
/// style those are based on or linked to, each as its `w:style` element;
/// with the document defaults (`w:docDefaults`).
pub(crate) fn styles_in_use(
    styles: &xmltree::Element,
    content: &[xmltree::Element],
) -> Vec<xmltree::Element> {
    fn names(element: &xmltree::Element, out: &mut std::collections::BTreeSet<String>) {
        if element.namespace.as_deref() == Some(WORDPROCESSINGML)
            && matches!(
                element.name.as_str(),
                "pStyle" | "rStyle" | "tblStyle" | "numStyleLink" | "styleLink"
            )
            && let Some(value) = element_value(element)
        {
            out.insert(value);
        }
        for node in &element.children {
            if let xmltree::XMLNode::Element(child) = node {
                names(child, out);
            }
        }
    }
    let mut wanted = std::collections::BTreeSet::new();
    for part in content {
        names(part, &mut wanted);
    }
    let definitions: Vec<&xmltree::Element> = styles
        .children
        .iter()
        .filter_map(|node| match node {
            xmltree::XMLNode::Element(e) if is_w(e, "style") || is_w(e, "docDefaults") => Some(e),
            _ => None,
        })
        .collect();
    let id = |style: &xmltree::Element| {
        style
            .attributes
            .iter()
            .find(|(name, _)| {
                name.local_name == "styleId" && name.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .map(|(_, value)| value.clone())
    };
    for style in &definitions {
        let is_default = style.attributes.iter().any(|(name, value)| {
            name.local_name == "default" && matches!(value.as_str(), "1" | "true" | "on")
        });
        if is_default && let Some(id) = id(style) {
            wanted.insert(id);
        }
    }
    // Close over basedOn and link.
    loop {
        let mut grew = false;
        for style in &definitions {
            if id(style).is_some_and(|id| wanted.contains(&id)) {
                let mut more = std::collections::BTreeSet::new();
                for node in &style.children {
                    if let xmltree::XMLNode::Element(e) = node
                        && (is_w(e, "basedOn") || is_w(e, "link"))
                        && let Some(value) = element_value(e)
                    {
                        more.insert(value);
                    }
                }
                // A paragraph style's own numbering names its list styles.
                names(style, &mut more);
                for name in more {
                    grew |= wanted.insert(name);
                }
            }
        }
        if !grew {
            break;
        }
    }
    definitions
        .into_iter()
        .filter(|style| {
            is_w(style, "docDefaults") || id(style).is_some_and(|id| wanted.contains(&id))
        })
        .cloned()
        .collect()
}

/// The formatting runs held inside a paragraph's other held elements (a
/// simple field's result, an inline content control) resolve to through the
/// styles part, with their text, runs of equal formatting joined; and, for
/// an element whose own run names a character style (a note reference, a
/// field character), what that style gives. Text inside a nested paragraph
/// (a text box) is that paragraph's, not read here.
fn held_run_formatting(
    paragraph: &ParagraphNode,
    styles: &crate::styles::StyleDefinitions,
    side: Side<'_>,
) -> Vec<(String, String)> {
    fn runs<'e>(element: &'e xmltree::Element, out: &mut Vec<&'e xmltree::Element>) {
        for node in &element.children {
            let xmltree::XMLNode::Element(child) = node else {
                continue;
            };
            if is_w(child, "r") {
                out.push(child);
            } else if !(is_w(child, "p") || is_w(child, "txbxContent")) {
                runs(child, out);
            }
        }
    }
    let paragraph_style = paragraph_style_id(paragraph, styles);
    let resolve = |properties: Option<&xmltree::Element>| -> String {
        let Some(properties) = properties else {
            return marks_key(
                &styles.resolve(&Default::default(), None, paragraph_style),
                side,
            );
        };
        let character_style = properties.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(e) if is_w(e, "rStyle") => element_value(e),
            _ => None,
        });
        match crate::styles::parse_rpr_marks(properties) {
            Ok(direct) => marks_key(
                &styles.resolve(&direct, character_style.as_deref(), paragraph_style),
                side,
            ),
            Err(error) => format!("run properties do not read: {error}"),
        }
    };
    let mut held: Vec<(String, String)> = Vec::new();
    let mut push = |key: String, text: &str| match held.last_mut() {
        Some((previous, joined)) if *previous == key => joined.push_str(text),
        _ => held.push((key, text.to_string())),
    };
    for inline in paragraph.segments.iter().flat_map(|s| &s.inlines) {
        let (raw, wrapper) = match inline {
            InlineNode::OpaqueInline(opaque) => {
                if matches!(opaque.kind, crate::domain::OpaqueKind::Hyperlink(_)) {
                    continue;
                }
                (opaque.raw_xml.as_deref(), Some(&opaque.wrapper_style_props))
            }
            InlineNode::Decoration(decoration) => (None, Some(&decoration.wrapper_style_props)),
            _ => continue,
        };
        if let Some(character_style) = wrapper.and_then(|props| props.char_style_id.as_deref()) {
            let key = marks_key(
                &styles.resolve(&Default::default(), Some(character_style), paragraph_style),
                side,
            );
            push(key, "\u{fffc}");
        }
        let Some(element) = raw.and_then(|raw| crate::word_xml::parse_raw_fragment(raw).ok())
        else {
            continue;
        };
        let mut found = Vec::new();
        runs(&element, &mut found);
        for run in found {
            let text: String = run
                .children
                .iter()
                .filter_map(|node| match node {
                    xmltree::XMLNode::Element(t) if is_w(t, "t") => Some(
                        t.children
                            .iter()
                            .filter_map(|n| match n {
                                xmltree::XMLNode::Text(text) => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect();
            if text.is_empty() {
                continue;
            }
            let properties = run.children.iter().find_map(|node| match node {
                xmltree::XMLNode::Element(e) if is_w(e, "rPr") => Some(e),
                _ => None,
            });
            push(resolve(properties), &text);
        }
    }
    held
}

/// The fonts a font table part (`w:fonts`, §17.8.3.11) embeds, as
/// `font name/face`, sorted.
pub(crate) fn embedded_fonts(bytes: &[u8]) -> Result<Vec<String>, String> {
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("font table does not parse: {error}"))?;
    let mut fonts = Vec::new();
    for node in &root.children {
        let xmltree::XMLNode::Element(font) = node else {
            continue;
        };
        if !is_w(font, "font") {
            continue;
        }
        let name = font
            .attributes
            .iter()
            .find(|(attribute, _)| {
                attribute.local_name == "name"
                    && attribute.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        for child in &font.children {
            if let xmltree::XMLNode::Element(face) = child
                && face.namespace.as_deref() == Some(WORDPROCESSINGML)
                && face.name.starts_with("embed")
            {
                fonts.push(format!("{name}/{}", face.name));
            }
        }
    }
    fonts.sort();
    Ok(fonts)
}

/// Whether held XML has a tab character (`w:tab` in a run, §17.3.3.32)
/// outside a nested paragraph. XML that does not parse may: its paragraph's
/// tab stops are then compared.
fn holds_tab(raw: &[u8]) -> bool {
    fn walk(element: &xmltree::Element, in_run: bool) -> bool {
        element.children.iter().any(|node| match node {
            xmltree::XMLNode::Element(child) => {
                if in_run && is_w(child, "tab") {
                    return true;
                }
                if is_w(child, "p") || is_w(child, "txbxContent") {
                    return false;
                }
                walk(child, is_w(child, "r"))
            }
            _ => false,
        })
    }
    crate::word_xml::parse_raw_fragment(raw)
        .map(|element| walk(&element, is_w(&element, "r")))
        .unwrap_or(true)
}

/// Each element and attribute of a known namespace under that namespace's
/// usual prefix: which prefix a writer binds a namespace to, or whether it
/// makes it the default namespace, names nothing (Namespaces in XML 1.0 §6).
fn canonical_prefixes(element: &mut xmltree::Element) {
    const KNOWN: &[(&str, &str)] = &[
        (WORDPROCESSINGML, "w"),
        (RELATIONSHIPS, "r"),
        (DRAWINGML, "a"),
        (
            "http://schemas.openxmlformats.org/drawingml/2006/picture",
            "pic",
        ),
        (
            "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
            "wp",
        ),
        (
            "http://schemas.openxmlformats.org/drawingml/2006/chart",
            "c",
        ),
        (
            "http://schemas.openxmlformats.org/markup-compatibility/2006",
            "mc",
        ),
        (
            "http://schemas.openxmlformats.org/officeDocument/2006/math",
            "m",
        ),
        (
            "http://schemas.microsoft.com/office/word/2010/wordprocessingShape",
            "wps",
        ),
        (
            "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup",
            "wpg",
        ),
        (
            "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
            "wp14",
        ),
        (
            "http://schemas.microsoft.com/office/word/2010/wordml",
            "w14",
        ),
        (
            "http://schemas.microsoft.com/office/word/2012/wordml",
            "w15",
        ),
        ("urn:schemas-microsoft-com:vml", "v"),
        (VML_OFFICE, "o"),
        ("urn:schemas-microsoft-com:office:word", "w10"),
    ];
    let usual = |namespace: Option<&str>| {
        let namespace = namespace?;
        KNOWN
            .iter()
            .find(|(uri, _)| *uri == namespace)
            .map(|(_, prefix)| (*prefix).to_string())
    };
    if let Some(prefix) = usual(element.namespace.as_deref()) {
        element.prefix = Some(prefix);
    }
    let attributes = std::mem::take(&mut element.attributes);
    for (mut name, value) in attributes {
        if let Some(prefix) = usual(name.namespace.as_deref()) {
            name.prefix = Some(prefix);
        }
        element.attributes.insert(name, value);
    }
    element.namespaces = None;
    for child in &mut element.children {
        if let xmltree::XMLNode::Element(child) = child {
            canonical_prefixes(child);
        }
    }
}

/// What each table style of a styles part gives a table (§17.7.6), along
/// its `w:basedOn` chain, nearest style first: per part of the table it
/// formats (`whole` for the style's own `w:rPr`, `w:pPr`, `w:tblPr`,
/// `w:trPr` and `w:tcPr`; the `w:type` of each `w:tblStylePr` for the
/// conditional parts, `firstRow`, `band1Horz` and the like), the canonical
/// XML. A style with the id `TableNormal` gives nothing: Word ignores the
/// children of a style with that id (MS-OI29500 §2.1.242). Also the id of
/// the default table style, which a table without `w:tblStyle` takes.
pub(crate) fn table_styles(bytes: &[u8]) -> Result<TableStyles, String> {
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("styles part does not parse: {error}"))?;
    let attribute = |element: &xmltree::Element, local: &str| {
        element
            .attributes
            .iter()
            .find(|(name, _)| {
                name.local_name == local && name.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .map(|(_, value)| value.clone())
    };
    let spelling = |element: &xmltree::Element| {
        let mut element = element.clone();
        strip_bookkeeping(&mut element);
        sort_attributes(&mut element);
        String::from_utf8_lossy(&crate::word_xml::serialize_raw_fragment(&element)).into_owned()
    };
    // id -> (based on, own parts).
    let mut own: std::collections::HashMap<
        String,
        (Option<String>, std::collections::BTreeMap<String, String>),
    > = std::collections::HashMap::new();
    let mut default = None;
    for node in &root.children {
        let xmltree::XMLNode::Element(style) = node else {
            continue;
        };
        if !is_w(style, "style") || attribute(style, "type").as_deref() != Some("table") {
            continue;
        }
        let Some(id) = attribute(style, "styleId") else {
            continue;
        };
        if matches!(
            attribute(style, "default").as_deref(),
            Some("1" | "true" | "on")
        ) {
            default = Some(id.clone());
        }
        let mut parts: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        let mut based_on = None;
        if id != "TableNormal" {
            for child in &style.children {
                let xmltree::XMLNode::Element(child) = child else {
                    continue;
                };
                if is_w(child, "basedOn") {
                    based_on = element_value(child);
                } else if ["rPr", "pPr", "tblPr", "trPr", "tcPr"]
                    .iter()
                    .any(|local| is_w(child, local))
                {
                    parts
                        .entry("whole".to_string())
                        .or_default()
                        .push_str(&spelling(child));
                } else if is_w(child, "tblStylePr")
                    && let Some(kind) = attribute(child, "type")
                {
                    parts.entry(kind).or_default().push_str(&spelling(child));
                }
            }
        }
        // Word takes the last definition of a repeated style id.
        own.insert(id, (based_on, parts));
    }
    let mut styles = std::collections::HashMap::new();
    for id in own.keys() {
        let mut chain: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = Some(id.clone());
        while let Some(style) = current {
            if !seen.insert(style.clone()) {
                break;
            }
            let Some((based_on, parts)) = own.get(&style) else {
                break;
            };
            for (part, xml) in parts {
                let entry = chain.entry(part.clone()).or_default();
                entry.push('|');
                entry.push_str(xml);
            }
            current = based_on.clone();
        }
        styles.insert(id.clone(), chain);
    }
    Ok(TableStyles { styles, default })
}

/// See [`table_styles`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TableStyles {
    pub(crate) styles:
        std::collections::HashMap<String, std::collections::BTreeMap<String, String>>,
    pub(crate) default: Option<String>,
}

/// What a table takes from its table style, for the parts its look
/// (`w:tblLook`, §17.4.56) turns on: the whole table always, and first and
/// last rows and columns, row and column bands and corner cells as the look
/// says. A table without a look takes every part.
fn table_style_in_effect(table: &crate::domain::TableNode, side: Side<'_>) -> Option<String> {
    let facts = side.parts?;
    let style = table
        .formatting
        .style_id
        .as_deref()
        .map(str::to_string)
        .or_else(|| facts.table_styles.default.clone())?;
    let Some(parts) = facts.table_styles.styles.get(&style) else {
        return Some(format!("{style}: not defined"));
    };
    let look = table.formatting.tbl_look.as_ref();
    let on = |part: &str| -> bool {
        let Some(look) = look else {
            return true;
        };
        match part {
            "whole" | "wholeTable" => true,
            "firstRow" => look.first_row,
            "lastRow" => look.last_row,
            "firstCol" => look.first_column,
            "lastCol" => look.last_column,
            "band1Horz" | "band2Horz" => !look.no_h_band,
            "band1Vert" | "band2Vert" => !look.no_v_band,
            "nwCell" => look.first_row && look.first_column,
            "neCell" => look.first_row && look.last_column,
            "swCell" => look.last_row && look.first_column,
            "seCell" => look.last_row && look.last_column,
            _ => true,
        }
    };
    Some(
        parts
            .iter()
            .filter(|(part, _)| on(part))
            .map(|(part, xml)| format!("{part}={xml}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Differences in what the tables of two blocks take from their table
/// styles ([`table_style_in_effect`]), tables taken in order, nested ones
/// included. Empty where the styles parts are the same or either side's
/// package is not known.
pub(crate) fn table_style_differences(
    before: &TrackedBlock,
    after: &TrackedBlock,
    before_side: Side<'_>,
    after_side: Side<'_>,
) -> Vec<crate::roundtrip_compare::Difference> {
    let (Some(before_facts), Some(after_facts)) = (before_side.parts, after_side.parts) else {
        return Vec::new();
    };
    if before_facts.styles_digest.is_some()
        && before_facts.styles_digest == after_facts.styles_digest
    {
        return Vec::new();
    }
    fn tables<'b>(block: &'b BlockNode, out: &mut Vec<&'b crate::domain::TableNode>) {
        if let BlockNode::Table(table) = block {
            out.push(table);
            for nested in table
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .flat_map(|cell| &cell.blocks)
            {
                tables(nested, out);
            }
        }
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    tables(&before.block, &mut left);
    tables(&after.block, &mut right);
    if left.len() != right.len() {
        return Vec::new();
    }
    left.iter()
        .zip(&right)
        .enumerate()
        .filter_map(|(index, (a, b))| {
            let (style_a, style_b) = (
                table_style_in_effect(a, before_side),
                table_style_in_effect(b, after_side),
            );
            (style_a != style_b).then(|| crate::roundtrip_compare::Difference {
                path: format!("block.tables[{index}].table_style"),
                left: format!("{style_a:?}"),
                right: format!("{style_b:?}"),
            })
        })
        .collect()
}

/// Per paragraph style of a styles part, the paragraph properties its
/// `w:basedOn` chain writes, nearest style first, then the document
/// defaults' (`w:pPrDefault`), in canonical XML: what a paragraph of the
/// style takes from it. Left out: tab stops and numbering (compared where a
/// paragraph uses them), the paragraph mark's run properties, and tracked
/// changes to the properties (`w:pPrChange`, a revision of its own). The
/// last definition of a repeated style id is the one in effect.
pub(crate) fn paragraph_style_properties(
    bytes: &[u8],
) -> Result<std::collections::HashMap<String, String>, String> {
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("styles part does not parse: {error}"))?;
    let child = |element: &xmltree::Element, local: &str| -> Option<xmltree::Element> {
        element.children.iter().find_map(|node| match node {
            xmltree::XMLNode::Element(e) if is_w(e, local) => Some(e.clone()),
            _ => None,
        })
    };
    let own_ppr = |container: &xmltree::Element| -> String {
        let Some(mut ppr) = child(container, "pPr") else {
            return String::new();
        };
        ppr.children.retain(|node| {
            !matches!(node, xmltree::XMLNode::Element(e)
                if ["tabs", "numPr", "rPr", "pPrChange"].iter().any(|local| is_w(e, local)))
        });
        strip_bookkeeping(&mut ppr);
        sort_attributes(&mut ppr);
        String::from_utf8_lossy(&crate::word_xml::serialize_raw_fragment(&ppr)).into_owned()
    };
    let defaults = child(&root, "docDefaults")
        .and_then(|defaults| child(&defaults, "pPrDefault"))
        .map(|default| own_ppr(&default))
        .unwrap_or_default();
    let mut own: std::collections::HashMap<String, (Option<String>, String)> =
        std::collections::HashMap::new();
    for node in &root.children {
        let xmltree::XMLNode::Element(style) = node else {
            continue;
        };
        let attribute = |local: &str| {
            style
                .attributes
                .iter()
                .find(|(name, _)| {
                    name.local_name == local && name.namespace.as_deref() == Some(WORDPROCESSINGML)
                })
                .map(|(_, value)| value.clone())
        };
        if !is_w(style, "style")
            || attribute("type").as_deref().unwrap_or("paragraph") != "paragraph"
        {
            continue;
        }
        let Some(id) = attribute("styleId") else {
            continue;
        };
        let based_on = child(style, "basedOn").and_then(|e| element_value(&e));
        own.insert(id, (based_on, own_ppr(style)));
    }
    let mut chains = std::collections::HashMap::new();
    for id in own.keys() {
        let mut chain = String::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = Some(id.clone());
        while let Some(style) = current {
            if !seen.insert(style.clone()) {
                break;
            }
            let Some((based_on, ppr)) = own.get(&style) else {
                break;
            };
            chain.push_str(ppr);
            chain.push('|');
            current = based_on.clone();
        }
        chain.push_str(&defaults);
        chains.insert(id.clone(), chain);
    }
    Ok(chains)
}

/// The separators of a notes part (`w:footnote`/`w:endnote` with
/// `w:type` `separator`, `continuationSeparator` or `continuationNotice`,
/// §17.11.23): per type, the canonical XML of its content. Word draws them
/// between the text and the notes.
pub(crate) fn note_separators(bytes: &[u8], family: &str) -> Result<Vec<(String, String)>, String> {
    let xml = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root = crate::word_xml::parse_raw_fragment(xml)
        .map_err(|error| format!("notes part does not parse: {error}"))?;
    let mut separators = Vec::new();
    for node in &root.children {
        let xmltree::XMLNode::Element(note) = node else {
            continue;
        };
        let kind = note
            .attributes
            .iter()
            .find(|(name, _)| {
                name.local_name == "type" && name.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
            .map(|(_, value)| value.clone());
        let Some(kind) = kind.filter(|kind| kind != "normal") else {
            continue;
        };
        let mut content = note.clone();
        content.attributes.clear();
        strip_bookkeeping(&mut content);
        strip_identities(&mut content);
        canonical_prefixes(&mut content);
        sort_attributes(&mut content);
        canonical_runs(&mut content);
        separators.push((
            format!("{family}/{kind}"),
            String::from_utf8_lossy(&crate::word_xml::serialize_raw_fragment(&content))
                .into_owned(),
        ));
    }
    separators.sort();
    Ok(separators)
}

/// The paragraph styles the `STYLEREF` fields of `parts` name (§17.16.5.65),
/// each with what it resolves to in the styles part: the style id, or
/// `missing`. A field shows the text of the nearest paragraph in the style
/// it names; Word recomputes those in headers and footers as it lays them
/// out, so a name that no longer resolves shows an error there.
pub(crate) fn styleref_targets(
    parts: &[xmltree::Element],
    styles: Option<&xmltree::Element>,
) -> Vec<(String, String)> {
    fn instructions(element: &xmltree::Element, current: &mut String, out: &mut Vec<String>) {
        if is_w(element, "fldSimple")
            && let Some((_, instr)) = element.attributes.iter().find(|(name, _)| {
                name.local_name == "instr" && name.namespace.as_deref() == Some(WORDPROCESSINGML)
            })
        {
            out.push(instr.clone());
        }
        if is_w(element, "instrText") {
            for node in &element.children {
                if let xmltree::XMLNode::Text(text) = node {
                    current.push_str(text);
                }
            }
        }
        if is_w(element, "p") {
            let mut paragraph = String::new();
            for node in &element.children {
                if let xmltree::XMLNode::Element(child) = node {
                    instructions(child, &mut paragraph, out);
                }
            }
            out.push(paragraph);
            return;
        }
        for node in &element.children {
            if let xmltree::XMLNode::Element(child) = node {
                instructions(child, current, out);
            }
        }
    }
    let mut texts = Vec::new();
    for part in parts {
        let mut loose = String::new();
        instructions(part, &mut loose, &mut texts);
        texts.push(loose);
    }
    let mut names = std::collections::BTreeSet::new();
    for text in texts {
        let mut rest = text.as_str();
        while let Some(at) = rest.find("STYLEREF") {
            rest = rest[at + "STYLEREF".len()..].trim_start();
            let name = if let Some(quoted) = rest.strip_prefix('"') {
                quoted.split('"').next().unwrap_or("").to_string()
            } else {
                rest.split_whitespace().next().unwrap_or("").to_string()
            };
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    let resolve = |name: &str| -> String {
        let Some(styles) = styles else {
            return "missing".to_string();
        };
        let mut found = None;
        for node in &styles.children {
            let xmltree::XMLNode::Element(style) = node else {
                continue;
            };
            if !is_w(style, "style") {
                continue;
            }
            let id = style
                .attributes
                .iter()
                .find(|(n, _)| n.local_name == "styleId")
                .map(|(_, v)| v.clone());
            let style_name = style.children.iter().find_map(|node| match node {
                xmltree::XMLNode::Element(e) if is_w(e, "name") => element_value(e),
                _ => None,
            });
            if style_name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
                || id.as_deref() == Some(name)
            {
                found = id;
            }
        }
        found.unwrap_or_else(|| "missing".to_string())
    };
    names
        .into_iter()
        .map(|name| {
            let target = resolve(&name);
            (name, target)
        })
        .collect()
}

const MATH: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Adjacent math runs (`m:r`, ECMA-376 Part 1 §22.1.2.87) of identical
/// properties joined into one, as text runs are: a math run's boundary
/// is where a writer split its text, not a change of how it looks.
fn join_math_runs(element: &mut xmltree::Element) {
    /// The properties and text of a math run that holds only `m:t`.
    fn math_run(node: &xmltree::XMLNode) -> Option<(Vec<&xmltree::Element>, String)> {
        let xmltree::XMLNode::Element(run) = node else {
            return None;
        };
        if !(run.name == "r" && run.namespace.as_deref() == Some(MATH)) {
            return None;
        }
        let mut properties = Vec::new();
        let mut text = String::new();
        let mut pieces = 0;
        for child in &run.children {
            let xmltree::XMLNode::Element(e) = child else {
                continue;
            };
            if e.name == "t" && e.namespace.as_deref() == Some(MATH) {
                pieces += 1;
                for node in &e.children {
                    match node {
                        xmltree::XMLNode::Text(t) => text.push_str(t),
                        _ => return None,
                    }
                }
            } else if e.name == "rPr" {
                properties.push(e);
            } else {
                return None;
            }
        }
        (pieces == 1).then_some((properties, text))
    }
    let mut joined: Vec<xmltree::XMLNode> = Vec::with_capacity(element.children.len());
    for child in std::mem::take(&mut element.children) {
        if let (Some((previous_properties, previous_text)), Some((properties, text))) =
            (joined.last().and_then(math_run), math_run(&child))
            && previous_properties == properties
        {
            let mut run = match &child {
                xmltree::XMLNode::Element(run) => run.clone(),
                _ => unreachable!("math_run read an element"),
            };
            let combined = format!("{previous_text}{text}");
            for node in &mut run.children {
                if let xmltree::XMLNode::Element(e) = node
                    && e.name == "t"
                    && e.namespace.as_deref() == Some(MATH)
                {
                    e.children = vec![xmltree::XMLNode::Text(combined.clone())];
                }
            }
            *joined.last_mut().expect("a previous math run") = xmltree::XMLNode::Element(run);
            continue;
        }
        joined.push(child);
    }
    element.children = joined;
}
