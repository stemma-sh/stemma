//! `InsertImage` / `ReplaceImage` — author binary image media into the package.
//!
//! Unlike [`super::images`] (`SetImageAttributes`, which only mutates a drawing's
//! *display* attributes in `raw_xml`), these verbs add a real `word/media/*`
//! binary part and an image relationship. The pure verb core has no
//! [`crate::docx_package::DocxPackage`] in scope, so it cannot register a part
//! itself. Instead it:
//!
//!  1. synthesizes (InsertImage) or rewrites (ReplaceImage) the drawing IR so the
//!     `a:blip r:embed` references a **logical rId** — a placeholder relationship
//!     id, never a real one;
//!  2. stages a [`PendingMedia`] carrying the bytes + that SAME logical rId.
//!
//! At save time `runtime::apply_pending_media` writes the binary, registers the
//! image relationship, and rewrites every `r:embed="<logical_rid>"` in the IR to
//! the real rId the package assigned. Using one shared logical-rId string is the
//! contract that links the verb to the save path: the drawing and the staged
//! media point at the same placeholder, so they get rewritten together to the
//! same real rId, leaving no orphan.
//!
//! ## Logical rId convention
//!
//! The logical rId MUST start with `"rId"` so [`crate::diff::find_blip_rid`]
//! (which the save-path rewrite and the inserted-rId collector both use) will
//! recognize it as a relationship id. We derive it deterministically from the
//! image's content digest so replaying the same transaction stages the same
//! placeholder.
//!
//! ## Tracking semantics
//!
//! - **InsertImage** is tracked (TrackedChange mode): the drawing rides in its
//!   own `Inserted` segment. accept-all keeps the drawing (referencing the now
//!   registered media); reject-all drops the inserted segment, so the media part
//!   is left unreferenced (harmless — an orphan part is fine; an orphan *rId* is
//!   not). In Direct mode the drawing is appended as a Normal segment.
//! - **ReplaceImage** is direct/untracked (like `SetImageAttributes`): OOXML has
//!   no tracked-change envelope for swapping a drawing's media. The old media
//!   part is left unreferenced (harmless).
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - empty image bytes                 → `ImageBytesEmpty`
//! - format/magic-byte mismatch        → `UnsupportedImageFormat`
//! - InsertImage target not a paragraph→ `NotAParagraph`
//! - ReplaceImage target not a drawing → `NotADrawing` / `DrawingNotFound`

use super::super::{EditError, MaterializationMode, find_block_index, validate_block_is_editable};
use super::images::find_descendant_by_local_mut;
use crate::domain::{
    BlockNode, CanonDoc, DocPart, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind, ParagraphNode,
    ProofRef, RevisionInfo, StyleProps, TrackedSegment, TrackingStatus,
};
use crate::edit::PendingMedia;
use crate::import::sha256_hex;
use crate::semantic_hash::check_block_guard;
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};

/// Fractional tolerance for the replace-image aspect-ratio guard. A replacement
/// whose intrinsic aspect ratio differs from the requested display extent's by
/// more than this is treated as a deliberate stretch and refused unless the
/// caller opts in (`allow_stretch`). 1% absorbs rounding between pixel
/// dimensions and EMU extents without letting a visible distortion through.
const ASPECT_EPSILON: f64 = 0.01;

/// Raster image formats we can author. The caller states the format; we
/// sanity-check it against the byte magic so a mislabeled blob is rejected at the
/// edge rather than written as a corrupt part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
}

impl ImageFormat {
    /// OPC content type (`Default`/`Override` ContentType for the part).
    pub fn content_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
        }
    }

    /// Lowercase extension without the dot, for the media part name.
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Gif => "gif",
        }
    }

    /// True when `bytes` begins with this format's signature. We check only the
    /// leading magic bytes — enough to catch a mislabeled blob without parsing
    /// the whole image.
    fn matches_magic(self, bytes: &[u8]) -> bool {
        match self {
            // PNG: 89 50 4E 47 0D 0A 1A 0A
            ImageFormat::Png => {
                bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
            }
            // JPEG: FF D8 FF
            ImageFormat::Jpeg => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
            // GIF: "GIF87a" or "GIF89a"
            ImageFormat::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        }
    }

    /// The image's intrinsic pixel dimensions `(width, height)`, decoded from the
    /// header bytes. Used to refuse a stretch when a replacement's aspect ratio
    /// disagrees with the requested display extent (`apply_replace`). Returns
    /// `None` when the header is truncated or malformed — the caller then fails
    /// loud rather than guessing a size (CLAUDE.md "no silent fallbacks").
    ///
    /// No image-decoding dependency: each format stores its dimensions at a fixed
    /// header offset.
    /// - **PNG** (§ IHDR): the 8-byte signature is followed by the IHDR chunk
    ///   (4-byte length + "IHDR" tag), then width and height as big-endian `u32`
    ///   at byte offsets 16 and 20.
    /// - **GIF** (logical screen descriptor): width and height as little-endian
    ///   `u16` at offsets 6 and 8.
    /// - **JPEG**: scan the segment markers for the first Start-Of-Frame
    ///   (`0xFFC0`–`0xFFCF`, excluding the non-SOF markers `0xC4` DHT, `0xC8`
    ///   JPG, `0xCC` DAC); height then width are big-endian `u16` at offsets 5
    ///   and 7 within that segment.
    ///
    /// Also drives the `insert_image` intrinsic-size default (`edit_v4`): when the
    /// caller omits `cx`/`cy` the display extent is derived from these dimensions.
    pub(crate) fn intrinsic_dimensions(self, bytes: &[u8]) -> Option<(u32, u32)> {
        match self {
            ImageFormat::Png => {
                // signature(8) + len(4) + "IHDR"(4) + width(4) + height(4) = 24.
                if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
                    return None;
                }
                let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
                let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
                Some((w, h))
            }
            ImageFormat::Gif => {
                // signature(6) + width(2 LE) + height(2 LE).
                if bytes.len() < 10 {
                    return None;
                }
                let w = u16::from_le_bytes(bytes[6..8].try_into().ok()?);
                let h = u16::from_le_bytes(bytes[8..10].try_into().ok()?);
                Some((w as u32, h as u32))
            }
            ImageFormat::Jpeg => {
                // Walk the marker segments from just after the SOI (0xFFD8).
                let mut i = 2;
                while i + 1 < bytes.len() {
                    // Markers are 0xFF followed by a non-0x00, non-0xFF type byte
                    // (0xFF padding bytes may precede the type).
                    if bytes[i] != 0xFF {
                        i += 1;
                        continue;
                    }
                    let mut marker = bytes[i + 1];
                    let mut j = i + 1;
                    while marker == 0xFF && j + 1 < bytes.len() {
                        j += 1;
                        marker = bytes[j];
                    }
                    // Standalone markers carry no length payload: SOI/EOI and the
                    // RSTn restart markers (0xD0–0xD9). Skip past them.
                    if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                        i = j + 1;
                        continue;
                    }
                    // Every other marker is followed by a 2-byte big-endian
                    // segment length (including the length bytes themselves).
                    let seg_start = j + 1;
                    if seg_start + 1 >= bytes.len() {
                        return None;
                    }
                    let seg_len =
                        u16::from_be_bytes(bytes[seg_start..seg_start + 2].try_into().ok()?)
                            as usize;
                    // SOF markers carry the frame dimensions. 0xC4 (DHT), 0xC8
                    // (JPG extension) and 0xCC (DAC) live in the same numeric
                    // range but are NOT frame headers.
                    let is_sof = (0xC0..=0xCF).contains(&marker)
                        && marker != 0xC4
                        && marker != 0xC8
                        && marker != 0xCC;
                    if is_sof {
                        // SOF payload: precision(1), height(2 BE), width(2 BE), …
                        // → height at seg_start+3, width at seg_start+5.
                        if seg_start + 7 > bytes.len() {
                            return None;
                        }
                        let h = u16::from_be_bytes(
                            bytes[seg_start + 3..seg_start + 5].try_into().ok()?,
                        );
                        let w = u16::from_be_bytes(
                            bytes[seg_start + 5..seg_start + 7].try_into().ok()?,
                        );
                        return Some((w as u32, h as u32));
                    }
                    // Advance past this segment to the next marker.
                    i = seg_start + seg_len;
                }
                None
            }
        }
    }
}

/// A validated image to insert or substitute: the binary bytes, the declared
/// format, the display box in EMUs, and optional alt text.
///
/// Construct via [`ImageSource::new`], which enforces non-empty bytes + a magic
/// match against `format` (no silent acceptance of a mislabeled blob).
#[derive(Clone, Debug)]
pub struct ImageSource {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    /// Display width in EMUs (`wp:extent` @cx, §20.4.2.7).
    pub cx_emu: i64,
    /// Display height in EMUs (`wp:extent` @cy).
    pub cy_emu: i64,
    /// Alt text for `wp:docPr` @descr (§20.4.2.5). `None` => no descr attribute.
    pub alt_text: Option<String>,
}

impl ImageSource {
    /// Validate the bytes against the declared format. Fails loud on empty bytes
    /// (`ImageBytesEmpty`) or a magic-byte mismatch (`UnsupportedImageFormat`) —
    /// the format is never inferred or defaulted from a mismatch.
    pub fn new(
        bytes: Vec<u8>,
        format: ImageFormat,
        cx_emu: i64,
        cy_emu: i64,
        alt_text: Option<String>,
        step_index: usize,
    ) -> Result<Self, EditError> {
        if bytes.is_empty() {
            return Err(EditError::ImageBytesEmpty { step_index });
        }
        if !format.matches_magic(&bytes) {
            return Err(EditError::UnsupportedImageFormat {
                declared: format.content_type(),
                step_index,
            });
        }
        Ok(ImageSource {
            bytes,
            format,
            cx_emu,
            cy_emu,
            alt_text,
        })
    }

    /// Deterministic logical rId for this image, derived from its content digest.
    /// Starts with `"rId"` so [`crate::diff::find_blip_rid`] recognizes it.
    fn logical_rid(&self) -> String {
        let digest = sha256_hex(&self.bytes);
        format!("rIdimg{}", &digest[..16])
    }

    /// The [`PendingMedia`] the save path materializes for this image.
    fn pending_media(&self, logical_rid: String) -> PendingMedia {
        let bytes_sha256 = sha256_hex(&self.bytes);
        PendingMedia {
            logical_rid,
            bytes: self.bytes.clone(),
            bytes_sha256,
            content_type: self.format.content_type().to_string(),
            ext: self.format.ext().to_string(),
        }
    }
}

/// XML-escape the few characters illegal inside an attribute value. Alt text is
/// caller-supplied; an unescaped `"` or `&` would corrupt the drawing fragment.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Build a minimal, well-formed inline `w:drawing` fragment that references
/// `logical_rid` via `a:blip r:embed`, sized to the source's extent, carrying the
/// alt text on `wp:docPr` @descr. All namespaces are declared locally so the
/// fragment round-trips standalone (matching how opaque drawings carry their
/// `raw_xml`). `doc_pr_id` is the `wp:docPr` @id (must be unique in the document
/// per §20.4.2.5; derived from the drawing node id).
fn build_drawing_xml(src: &ImageSource, logical_rid: &str, doc_pr_id: u32) -> Vec<u8> {
    let descr_attr = match &src.alt_text {
        Some(t) => format!(r#" descr="{}""#, escape_attr(t)),
        None => String::new(),
    };
    format!(
        r#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="{id}" name="Picture {id}"{descr}/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="{id}" name="Picture {id}"{descr}/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#,
        cx = src.cx_emu,
        cy = src.cy_emu,
        id = doc_pr_id,
        descr = descr_attr,
        rid = logical_rid,
    )
    .into_bytes()
}

/// Synthesize a fresh `OpaqueInline{Drawing}` whose `raw_xml` is `drawing_xml`.
fn synthesize_drawing_inline(id: &NodeId, drawing_xml: Vec<u8>) -> InlineNode {
    let content_hash = sha256_hex(&drawing_xml);
    InlineNode::from(OpaqueInlineNode {
        id: id.clone(),
        kind: OpaqueKind::Drawing,
        opaque_ref: format!("drawingref_{}", id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: id.clone(),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(drawing_xml),
        content_hash: Some(content_hash),
    })
}

/// Apply an `InsertImage` step: append the synthesized drawing to the target
/// paragraph (its own `Inserted` segment in TrackedChange mode, `Normal` in
/// Direct), and stage the [`PendingMedia`] so the save path registers the binary
/// and rewrites the logical rId.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_insert(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: Option<&str>,
    semantic_hash: Option<&str>,
    image: &ImageSource,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
    pending: &mut Vec<PendingMedia>,
) -> Result<(), EditError> {
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // The host paragraph must be editable (Normal status, no existing tracked
    // segments) and actually a paragraph.
    validate_block_is_editable(&doc.blocks[idx], step_index)?;
    match &doc.blocks[idx].block {
        BlockNode::Paragraph(_) => {}
        BlockNode::Table(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "table",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "opaque_block",
                step_index,
            });
        }
    }

    if let Some(expected) = semantic_hash
        && let Err(actual) = check_block_guard(&doc.blocks[idx].block, expected)
    {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: expected.to_string(),
            actual,
            step_index,
        });
    }

    let BlockNode::Paragraph(para) = &mut doc.blocks[idx].block else {
        unreachable!("checked paragraph above");
    };

    let logical_rid = image.logical_rid();
    let drawing_id = NodeId::from(format!("{}_img0", para.id.0));
    // wp:docPr @id must be a positive integer unique in the document. Derive it
    // from the logical rId digest so it is stable across replays and unlikely to
    // collide with existing drawings.
    let doc_pr_id = doc_pr_id_from(&logical_rid);
    let drawing_xml = build_drawing_xml(image, &logical_rid, doc_pr_id);
    let drawing = synthesize_drawing_inline(&drawing_id, drawing_xml);

    append_drawing_segment(para, drawing, revision, mode, expect, step_index)?;

    pending.push(image.pending_media(logical_rid));
    Ok(())
}

/// Append the drawing as a new trailing segment, or splice it after `expect` if
/// an anchor is supplied. v1 supports the simple append (anchor `None`); a
/// supplied anchor must be found (`ExpectMismatch`) — we do NOT silently fall
/// back to appending.
fn append_drawing_segment(
    para: &mut ParagraphNode,
    drawing: InlineNode,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    expect: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    let status = match mode {
        MaterializationMode::TrackedChange => TrackingStatus::Inserted(revision.clone()),
        MaterializationMode::Direct => TrackingStatus::Normal,
    };

    if let Some(anchor) = expect {
        // Anchor-based placement: require the anchor to be present in the visible
        // text. v1 only supports appending after the segment that ends with the
        // anchor; if not found, fail loud.
        let visible: String = para
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        if !visible.contains(anchor) {
            return Err(EditError::ExpectMismatch {
                block_id: para.id.clone(),
                expected: anchor.to_string(),
                actual_text: visible,
                step_index,
            });
        }
    }

    para.segments.push(TrackedSegment {
        status,
        inlines: vec![drawing],
    });
    para.block_text_hash = None;
    para.rendered_text = None;
    Ok(())
}

/// Derive a positive `wp:docPr` @id from the logical rId's hex digest tail.
fn doc_pr_id_from(logical_rid: &str) -> u32 {
    let tail: String = logical_rid
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    let n = u32::from_str_radix(&tail.chars().rev().take(6).collect::<String>(), 16).unwrap_or(0);
    // wp:docPr @id must be >= 1.
    n.max(1)
}

/// A validated `ReplaceImage` request: the replacement [`ImageSource`] plus the
/// caller's opt-out of the aspect-ratio guard. Bundled so [`apply_replace`]
/// keeps a small argument list.
pub(crate) struct ReplaceRequest<'a> {
    pub image: &'a ImageSource,
    /// When `false`, a replacement whose intrinsic aspect ratio disagrees with
    /// the requested extent is refused (`ImageAspectMismatch`) rather than
    /// stretched.
    pub allow_stretch: bool,
}

/// Validate the replacement image before applying it. Two refusals:
///
/// 1. The header is magic-valid but its pixel dimensions cannot be decoded
///    (truncated / malformed) → `ImageHeaderUndecodable`. A header we can't read
///    is a corrupt image we're about to embed; refuse. This is NOT bypassable by
///    `allow_stretch` — that opts into stretching, not into corrupt bytes.
/// 2. The intrinsic aspect ratio disagrees with the requested display extent
///    beyond [`ASPECT_EPSILON`] → `ImageAspectMismatch`, unless `allow_stretch`.
fn check_replace_aspect(
    req: &ReplaceRequest<'_>,
    drawing_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    let image = req.image;

    // (1) Decode dimensions. `None` => an undecodable header — refuse before we
    // embed it, regardless of allow_stretch. A zero dimension is equally
    // unusable (we can't form an aspect ratio), so treat it as undecodable too.
    let (iw, ih) = match image.format.intrinsic_dimensions(&image.bytes) {
        Some((w, h)) if w > 0 && h > 0 => (w, h),
        _ => {
            return Err(EditError::ImageHeaderUndecodable {
                drawing_id: drawing_id.clone(),
                format: image.format.content_type(),
                len: image.bytes.len(),
                step_index,
            });
        }
    };

    // (2) Aspect guard. allow_stretch deliberately stretches, so it skips ONLY
    // this comparison (not the undecodable-header refusal above).
    if req.allow_stretch {
        return Ok(());
    }
    if image.cx_emu > 0 && image.cy_emu > 0 {
        let intrinsic = iw as f64 / ih as f64;
        let requested = image.cx_emu as f64 / image.cy_emu as f64;
        if (intrinsic - requested).abs() / intrinsic > ASPECT_EPSILON {
            return Err(EditError::ImageAspectMismatch {
                drawing_id: drawing_id.clone(),
                intrinsic_w: iw,
                intrinsic_h: ih,
                requested_cx: image.cx_emu,
                requested_cy: image.cy_emu,
                step_index,
            });
        }
    }
    Ok(())
}

/// Apply a `ReplaceImage` step: locate the existing drawing, rewrite its
/// `a:blip r:embed` to a new logical rId, APPLY the requested display extent
/// (`wp:extent` @cx/@cy), recompute `content_hash`, and stage the new media.
/// Direct/untracked. The old media part is left unreferenced.
///
/// The op already requires `cx`/`cy`; we now actually write them (previously
/// they were silently discarded — a "no silent fallbacks" violation). When the
/// requested extent's aspect ratio disagrees with the replacement's intrinsic
/// pixel aspect by more than [`ASPECT_EPSILON`], the call would stretch the
/// image: we refuse with [`EditError::ImageAspectMismatch`] unless
/// `req.allow_stretch` is set. The extent is written via the parse/serialize
/// fragment mutator (the same path `SetImageAttributes` uses), not a brittle
/// string replace.
pub(crate) fn apply_replace(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    drawing_id: &NodeId,
    semantic_hash: Option<&str>,
    req: &ReplaceRequest<'_>,
    step_index: usize,
    pending: &mut Vec<PendingMedia>,
) -> Result<(), EditError> {
    let image = req.image;
    // Aspect guard (before locating the node so a refusal touches nothing).
    check_replace_aspect(req, drawing_id, step_index)?;

    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    let node =
        super::images::locate_drawing_mut(&mut doc.blocks[idx].block, drawing_id, step_index)?;

    if let Some(expected) = semantic_hash {
        let actual = node.content_hash.as_deref().unwrap_or("");
        if actual != expected {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: drawing_id.clone(),
                expected: expected.to_string(),
                actual: actual.to_string(),
                step_index,
            });
        }
    }

    let raw = node
        .raw_xml
        .as_deref()
        .ok_or_else(|| EditError::DrawingMissingRawXml {
            drawing_id: drawing_id.clone(),
            step_index,
        })?;
    let mut element = parse_raw_fragment(raw).map_err(|e| EditError::DrawingRawXmlParse {
        drawing_id: drawing_id.clone(),
        reason: e.to_string(),
        step_index,
    })?;

    // The drawing must currently reference a blip rId; otherwise there is nothing
    // to swap and ReplaceImage is the wrong verb (a no-media drawing should use
    // InsertImage). Fail loud rather than silently leaving an unreferenced media.
    let blip = find_descendant_by_local_mut(&mut element, "blip").ok_or_else(|| {
        EditError::ImageAttributeTargetAbsent {
            drawing_id: drawing_id.clone(),
            attribute: "a:blip r:embed",
            step_index,
        }
    })?;
    if crate::xml_attrs::attr_get(blip, "r:embed").is_none() {
        return Err(EditError::ImageAttributeTargetAbsent {
            drawing_id: drawing_id.clone(),
            attribute: "a:blip r:embed",
            step_index,
        });
    }
    let new_rid = image.logical_rid();
    crate::xml_attrs::attr_set(blip, "r:embed", &new_rid);

    // Apply the requested display extent. `wp:extent` carries the inline-display
    // box; we write @cx/@cy there specifically (NOT the inner `a:ext` on the
    // graphic frame, which is keyed separately), mirroring `SetImageAttributes`.
    let extent = find_descendant_by_local_mut(&mut element, "extent").ok_or_else(|| {
        EditError::ImageAttributeTargetAbsent {
            drawing_id: drawing_id.clone(),
            attribute: "wp:extent",
            step_index,
        }
    })?;
    crate::xml_attrs::attr_set(extent, "cx", image.cx_emu.to_string());
    crate::xml_attrs::attr_set(extent, "cy", image.cy_emu.to_string());

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);

    pending.push(image.pending_media(new_rid));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(b"payload");
        v
    }

    #[test]
    fn image_source_rejects_empty_bytes() {
        let err = ImageSource::new(Vec::new(), ImageFormat::Png, 1, 1, None, 0).unwrap_err();
        assert!(
            matches!(err, EditError::ImageBytesEmpty { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn image_source_rejects_magic_mismatch() {
        // JPEG bytes declared as PNG.
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0x00];
        let err = ImageSource::new(jpeg, ImageFormat::Png, 1, 1, None, 0).unwrap_err();
        assert!(
            matches!(err, EditError::UnsupportedImageFormat { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn image_source_accepts_matching_png() {
        let src = ImageSource::new(png(), ImageFormat::Png, 10, 20, Some("logo".into()), 0)
            .expect("valid png");
        assert_eq!(src.format.content_type(), "image/png");
        assert!(
            src.logical_rid().starts_with("rId"),
            "logical rId convention"
        );
    }

    #[test]
    fn drawing_xml_references_logical_rid_and_alt_text() {
        let src = ImageSource::new(png(), ImageFormat::Png, 100, 200, Some("a & b".into()), 0)
            .expect("valid");
        let rid = src.logical_rid();
        let xml = String::from_utf8(build_drawing_xml(&src, &rid, 7)).unwrap();
        assert!(xml.contains(&format!(r#"r:embed="{rid}""#)));
        assert!(xml.contains(r#"cx="100""#));
        assert!(xml.contains(r#"cy="200""#));
        // alt text is escaped on docPr @descr.
        assert!(xml.contains(r#"descr="a &amp; b""#), "{xml}");
        // find_blip_rid must recover the same logical rId the media stages under.
        assert_eq!(
            crate::diff::find_blip_rid(&xml).as_deref(),
            Some(rid.as_str())
        );
    }

    #[test]
    fn pending_media_digest_matches_bytes() {
        let src = ImageSource::new(png(), ImageFormat::Png, 1, 1, None, 0).unwrap();
        let m = src.pending_media(src.logical_rid());
        assert_eq!(m.bytes_sha256, sha256_hex(&png()));
        assert_eq!(m.content_type, "image/png");
        assert_eq!(m.ext, "png");
    }

    // ── intrinsic_dimensions ────────────────────────────────────────────────

    /// A PNG with the IHDR `width`/`height` set to `w`/`h` (big-endian u32 at
    /// offsets 16/20). The rest of the IHDR can be anything; we only read the
    /// dimensions.
    fn png_wh(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, color type, …
        v
    }

    /// A GIF with the logical-screen-descriptor width/height (little-endian u16
    /// at offsets 6/8).
    fn gif_wh(w: u16, h: u16) -> Vec<u8> {
        let mut v = Vec::from(*b"GIF89a");
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&[0, 0, 0]); // packed fields, bg, aspect
        v
    }

    /// A minimal JPEG: SOI, an APP0 segment, then a baseline SOF0 carrying the
    /// height/width (big-endian u16). Exercises the marker walk past a non-SOF
    /// segment before reaching the frame header.
    fn jpeg_wh(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8]; // SOI
        // APP0: marker + length(16) + "JFIF\0" + version/units/density/thumb.
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(b"JFIF\0");
        v.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
        // SOF0: marker + length(17) + precision(8) + height + width + comps.
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v
    }

    #[test]
    fn intrinsic_dimensions_png() {
        assert_eq!(
            ImageFormat::Png.intrinsic_dimensions(&png_wh(800, 600)),
            Some((800, 600))
        );
    }

    #[test]
    fn intrinsic_dimensions_gif() {
        assert_eq!(
            ImageFormat::Gif.intrinsic_dimensions(&gif_wh(320, 240)),
            Some((320, 240))
        );
    }

    #[test]
    fn intrinsic_dimensions_jpeg() {
        assert_eq!(
            ImageFormat::Jpeg.intrinsic_dimensions(&jpeg_wh(1024, 768)),
            Some((1024, 768))
        );
    }

    #[test]
    fn intrinsic_dimensions_truncated_header_is_none() {
        // A PNG signature with no IHDR payload → no dimensions (fail loud at the
        // caller, never a guess).
        assert_eq!(ImageFormat::Png.intrinsic_dimensions(&png()), None);
        assert_eq!(ImageFormat::Gif.intrinsic_dimensions(b"GIF89a"), None);
        assert_eq!(ImageFormat::Jpeg.intrinsic_dimensions(&[0xFF, 0xD8]), None);
    }
}
