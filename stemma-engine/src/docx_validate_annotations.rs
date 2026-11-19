//! Annotation ID, document structure, and tracked-change content-model checks
//! for post-serialization DOCX validation.
//!
//! Each check function receives parsed XML elements and returns a list of
//! [`ValidationFinding`] values describing any violations.

use std::collections::HashMap;
use xmltree::{Element, XMLNode};

use crate::docx_validate::{ValidationFinding, ValidationSeverity};
use crate::xml_attrs::attr_get;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const WML_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Element local names whose `w:id` attribute is an annotation ID.
const ANNOTATION_ID_ELEMENTS: &[&str] = &[
    "del",
    "ins",
    "moveFrom",
    "moveTo",
    "bookmarkStart",
    "bookmarkEnd",
    "commentRangeStart",
    "commentRangeEnd",
    "pPrChange",
    "rPrChange",
    "tblPrChange",
    "tblPrExChange",
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
    "cellIns",
    "cellDel",
];

/// Revision carriers whose `w:id` is required by their WordprocessingML
/// content type. This is deliberately separate from annotation range markers:
/// it covers the native changes that Accept/Reject address, including property
/// and table-structure revisions outside `w:ins` / `w:del`.
const REQUIRED_TRACKED_CHANGE_ID_ELEMENTS: &[&str] = &[
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "pPrChange",
    "rPrChange",
    "tblPrChange",
    "tblPrExChange",
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
    "cellIns",
    "cellDel",
    "cellMerge",
    "numberingChange",
    "tblGridChange",
];

/// Direct children allowed in CT_RunTrackChange (w:ins/w:del in paragraph
/// context) per ECMA-376 §17.13.5.14/18 + Annex A schema.
///
/// CT_RunTrackChange = CT_TrackChange + (EG_ContentRunContent | m:EG_OMathMathElements)*
///
/// EG_ContentRunContent = customXml | smartTag | sdt | dir | bdo | r | EG_RunLevelElts
/// EG_RunLevelElts      = proofErr | permStart | permEnd | ins | del | moveFrom | moveTo
///                       | bookmarkStart | bookmarkEnd | commentRangeStart | commentRangeEnd
///                       | moveFromRangeStart | moveFromRangeEnd | moveToRangeStart | moveToRangeEnd
///                       | customXmlInsRangeStart | customXmlInsRangeEnd
///                       | customXmlDelRangeStart | customXmlDelRangeEnd
///                       | customXmlMoveFromRangeStart | customXmlMoveFromRangeEnd
///                       | customXmlMoveToRangeStart | customXmlMoveToRangeEnd
///                       | (+ m:EG_OMathMathElements via extension)
///
/// The direct-children check uses this allowlist. Additionally, a recursive
/// descendant check catches paragraph-level-only elements
/// (`FORBIDDEN_DESCENDANTS_IN_TRACKED_CHANGE`) that should never appear at
/// any depth inside a tracked change (unless behind a content boundary like
/// `w:txbxContent`).
const ALLOWED_IN_CT_RUN_TRACK_CHANGE: &[&str] = &[
    // EG_ContentRunContent
    "customXml",
    "smartTag",
    "sdt",
    "dir",
    "bdo",
    "r",
    // EG_RunLevelElts
    "proofErr",
    "permStart",
    "permEnd",
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "bookmarkStart",
    "bookmarkEnd",
    "commentRangeStart",
    "commentRangeEnd",
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "customXmlInsRangeStart",
    "customXmlInsRangeEnd",
    "customXmlDelRangeStart",
    "customXmlDelRangeEnd",
    "customXmlMoveFromRangeStart",
    "customXmlMoveFromRangeEnd",
    "customXmlMoveToRangeStart",
    "customXmlMoveToRangeEnd",
];

/// WML elements from EG_PContent that are NOT in EG_ContentRunContent.
/// These are paragraph-level-only elements that must never appear as
/// descendants inside a tracked change (w:ins/w:del), regardless of nesting
/// depth. Per ECMA-376, CT_RunTrackChange allows EG_ContentRunContent but
/// NOT EG_PContent extras.
///
/// EG_PContent = EG_ContentRunContent | fldSimple | hyperlink | subDoc
const FORBIDDEN_DESCENDANTS_IN_TRACKED_CHANGE: &[&str] = &["fldSimple", "hyperlink", "subDoc"];

/// WML elements that establish a new content scope. Recursion for the
/// forbidden-descendant check stops here, because these elements contain
/// their own paragraph-level content model.
const TRACKED_CHANGE_CONTENT_BOUNDARIES: &[&str] = &["txbxContent"];

/// Opaque content-control wrappers (EG_ContentRunContent members that are
/// preserved as opaque raw XML and can legally sit inside a `w:del` while
/// carrying their own runs). Run text inside one of these, under a `w:del`, must
/// use the deleted-text content model — this is the case Word crashes on
/// during accept-all. A *direct* deleted run (`<w:del><w:r><w:t>…`) is a
/// separately tolerated Word quirk and is NOT flagged; the serializer only
/// rewrites text inside opaque raw XML, so the check mirrors it exactly.
const DELETED_OPAQUE_RUN_WRAPPERS: &[&str] = &["sdt", "customXml", "smartTag"];

/// Elements that re-scope run-text form inside a `w:del` subtree, so the
/// deleted-text-form check (below) stops recursion at them:
/// - `w:txbxContent` is a separate story where `w:t` stays legal even under a
///   deleted drawing (verified against real Word);
/// - a nested tracked container (`w:ins`/`w:moveFrom`/`w:moveTo`) carries its
///   own text-form rule — inserted and moved text keep `w:t`, not `w:delText`.
///
/// This mirrors the serializer's `coerce_opaque_run_text` txbxContent
/// exemption so the validator flags exactly what the serializer must (and must
/// not) rewrite.
const DELETED_TEXT_FORM_BOUNDARIES: &[&str] = &["txbxContent", "ins", "moveFrom", "moveTo"];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the local name of an element, stripping any namespace prefix.
fn local_name(name: &str) -> &str {
    match name.find(':') {
        Some(pos) => &name[pos + 1..],
        None => name,
    }
}

/// Returns `true` if `element` is a WML element with the given local name.
fn is_w_tag(element: &Element, local: &str) -> bool {
    if element.name == local {
        if element.prefix.as_deref() == Some("w") {
            return true;
        }
        return element.namespace.as_deref() == Some(WML_NS);
    }
    element.name == format!("w:{local}")
}

/// Walk all descendant elements depth-first, calling `f` with each element
/// and the ancestor path (list of local tag names from root to parent).
fn walk_elements<F>(el: &Element, f: &mut F)
where
    F: FnMut(&Element, &[String]),
{
    let mut path = Vec::new();
    walk_elements_inner(el, &mut path, f);
}

fn walk_elements_inner<F>(el: &Element, path: &mut Vec<String>, f: &mut F)
where
    F: FnMut(&Element, &[String]),
{
    f(el, path);
    path.push(local_name(&el.name).to_string());
    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            walk_elements_inner(child_el, path, f);
        }
    }
    path.pop();
}

/// Recursively collect forbidden paragraph-level-only WML elements that
/// appear as non-direct descendants of a tracked change element. This
/// complements the direct-children check: direct children are already
/// validated against `ALLOWED_IN_CT_RUN_TRACK_CHANGE`, so this function
/// starts from grandchildren onward.
///
/// Stops recursion at content boundaries (e.g. `w:txbxContent`) which
/// establish their own paragraph-level content scope.
fn collect_forbidden_descendants(tracked_change_el: &Element, out: &mut Vec<String>) {
    // Iterate direct children — don't check them (the direct-children check
    // handles that), but recurse into their subtrees.
    for child in &tracked_change_el.children {
        let XMLNode::Element(child_el) = child else {
            continue;
        };
        collect_forbidden_in_subtree(child_el, out);
    }
}

fn collect_forbidden_in_subtree(el: &Element, out: &mut Vec<String>) {
    for child in &el.children {
        let XMLNode::Element(child_el) = child else {
            continue;
        };
        let child_local = local_name(&child_el.name);

        // Stop recursion at content boundaries — they have their own scope.
        let is_textbox_scope = child_local == "txbxContent";
        let is_wml_boundary =
            is_w_element(child_el) && TRACKED_CHANGE_CONTENT_BOUNDARIES.contains(&child_local);
        if is_textbox_scope || is_wml_boundary {
            continue;
        }

        // Check if this descendant is a forbidden paragraph-level element.
        if is_w_element(child_el) && FORBIDDEN_DESCENDANTS_IN_TRACKED_CHANGE.contains(&child_local)
        {
            out.push(child_local.to_string());
        }

        // Recurse into children.
        collect_forbidden_in_subtree(child_el, out);
    }
}

/// Collect run-text elements inside a `w:del` that use the wrong text form:
/// `w:t` (must be `w:delText`) and `w:instrText` (must be `w:delInstrText`),
/// per the I-TC-001 tracked-change content model (ECMA-376 §17.4.20 / §17.16.13)
/// — but ONLY when the text sits inside an opaque content-control wrapper
/// (`DELETED_OPAQUE_RUN_WRAPPERS`). That is the crashing case: an inline `w:sdt`
/// wrapped in a `w:del` whose inner runs keep `w:t` makes Word repair the file
/// and crashes accept-all. A direct `<w:del><w:r><w:t>…` is a separately
/// tolerated Word quirk (see the `del-text-variants` fixture) and is left alone,
/// matching the serializer, which only rewrites text inside opaque raw XML.
///
/// Recursion stops at `DELETED_TEXT_FORM_BOUNDARIES` and at forbidden
/// paragraph-level elements (reported separately). `in_opaque` is true once we
/// are inside one of the opaque wrappers. Returns the offending local names.
fn collect_deleted_text_form_violations(el: &Element, in_opaque: bool, out: &mut Vec<String>) {
    for child in &el.children {
        let XMLNode::Element(child_el) = child else {
            continue;
        };
        let child_local = local_name(&child_el.name);

        let is_textbox_scope = child_local == "txbxContent";
        let is_wml_boundary = is_w_element(child_el)
            // Text-form scope boundaries — w:t is legitimately allowed here.
            && (DELETED_TEXT_FORM_BOUNDARIES.contains(&child_local)
                // Paragraph-level-only elements are forbidden inside a tracked
                // change outright (reported by the forbidden-descendant check);
                // their inner text form is moot until they are removed, so we
                // don't descend and double-report it.
                || FORBIDDEN_DESCENDANTS_IN_TRACKED_CHANGE.contains(&child_local));
        if is_textbox_scope || is_wml_boundary {
            continue;
        }

        let child_in_opaque = in_opaque
            || (is_w_element(child_el) && DELETED_OPAQUE_RUN_WRAPPERS.contains(&child_local));

        if in_opaque && is_w_element(child_el) && (child_local == "t" || child_local == "instrText")
        {
            out.push(child_local.to_string());
        }

        collect_deleted_text_form_violations(child_el, child_in_opaque, out);
    }
}

// ---------------------------------------------------------------------------
// I-ANN-001 + I-ANN-002: Annotation ID uniqueness and validity
// ---------------------------------------------------------------------------

/// Record of a single annotation ID occurrence.
#[derive(Debug)]
struct AnnotationIdOccurrence {
    story_part: String,
    element_name: String,
}

/// Check that all annotation IDs (`w:id` on annotation elements) are unique
/// across all story parts, and that each value is a valid non-negative integer
/// (u32).
///
/// Returns findings for:
/// - **I-ANN-001**: duplicate annotation IDs across stories
/// - **I-ANN-002**: annotation IDs that are not valid u32 values
pub fn check_annotation_id_uniqueness(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // id_value -> list of occurrences
    let mut seen: HashMap<String, Vec<AnnotationIdOccurrence>> = HashMap::new();

    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            let el_local = local_name(&el.name);
            if !ANNOTATION_ID_ELEMENTS.contains(&el_local) {
                return;
            }

            let Some(id_value) = attr_get(el, "w:id") else {
                // Missing w:id on an annotation element — covered by I-TC-002
                // for del/ins, but we note it for all annotation elements too.
                return;
            };

            // I-ANN-002: validate the value is a u32
            if id_value.parse::<u32>().is_err() {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-002: annotation w:id value '{id_value}' on <{el_local}> \
                         is not a valid non-negative integer (u32)"
                    ),
                    location: format!("{part_path} <{el_local}>"),
                });
            }

            seen.entry(id_value.clone())
                .or_default()
                .push(AnnotationIdOccurrence {
                    story_part: part_path.clone(),
                    element_name: el_local.to_string(),
                });
        });
    }

    // I-ANN-001: report duplicates.
    // Exception: bookmarkStart and bookmarkEnd are *designed* to share the same
    // w:id (that's how they pair). A pair of {bookmarkStart, bookmarkEnd} with the
    // same ID in the same story part is not a duplicate — it's a valid pair.
    // Similarly, commentRangeStart/commentRangeEnd share IDs.
    const PAIRED_ELEMENTS: &[(&str, &str)] = &[
        ("bookmarkStart", "bookmarkEnd"),
        ("commentRangeStart", "commentRangeEnd"),
    ];

    for (id_value, occurrences) in &seen {
        if occurrences.len() <= 1 {
            continue;
        }

        // Check if this is a valid paired annotation (exactly 2 occurrences,
        // one start and one end, in the same story part).
        if occurrences.len() == 2 {
            let is_valid_pair = PAIRED_ELEMENTS.iter().any(|(start, end)| {
                let has_start = occurrences.iter().any(|o| o.element_name == *start);
                let has_end = occurrences.iter().any(|o| o.element_name == *end);
                let same_story = occurrences[0].story_part == occurrences[1].story_part;
                has_start && has_end && same_story
            });
            if is_valid_pair {
                continue;
            }
        }

        let locations: Vec<String> = occurrences
            .iter()
            .map(|o| format!("<{}> in {}", o.element_name, o.story_part))
            .collect();
        findings.push(ValidationFinding {
            rule_id: "I-ANN-001",
            severity: ValidationSeverity::Error,
            message: format!(
                "I-ANN-001: annotation w:id '{id_value}' is used {} times: {}",
                occurrences.len(),
                locations.join(", ")
            ),
            location: occurrences
                .first()
                .map(|o| format!("{} <{}>", o.story_part, o.element_name))
                .unwrap_or_default(),
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// I-ANN-003: Bookmark start/end pairing
// ---------------------------------------------------------------------------

/// Check that every `bookmarkStart` has a matching `bookmarkEnd` with the
/// same `w:id` within the same story part, and vice versa.
pub fn check_bookmark_pairing(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        let mut starts: HashMap<String, u32> = HashMap::new();
        let mut ends: HashMap<String, u32> = HashMap::new();

        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "bookmarkStart") {
                if let Some(id) = attr_get(el, "w:id") {
                    *starts.entry(id.clone()).or_insert(0) += 1;
                }
            } else if is_w_tag(el, "bookmarkEnd")
                && let Some(id) = attr_get(el, "w:id")
            {
                *ends.entry(id.clone()).or_insert(0) += 1;
            }
        });

        // Every start must have an end
        for (id, count) in &starts {
            let end_count = ends.get(id).copied().unwrap_or(0);
            if end_count == 0 {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-003",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-003: bookmarkStart w:id='{id}' has no matching \
                         bookmarkEnd in story part '{part_path}'"
                    ),
                    location: format!("{part_path} <bookmarkStart w:id=\"{id}\">"),
                });
            } else if end_count != *count {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-003",
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "I-ANN-003: bookmarkStart w:id='{id}' appears {count} time(s) \
                         but bookmarkEnd appears {end_count} time(s) in '{part_path}'"
                    ),
                    location: format!("{part_path} <bookmarkStart w:id=\"{id}\">"),
                });
            }
        }

        // Every end must have a start
        for id in ends.keys() {
            if !starts.contains_key(id) {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-003",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-003: bookmarkEnd w:id='{id}' has no matching \
                         bookmarkStart in story part '{part_path}'"
                    ),
                    location: format!("{part_path} <bookmarkEnd w:id=\"{id}\">"),
                });
            }
        }
    }

    findings
}

// ---------------------------------------------------------------------------
// I-ANN-004: paraId range validation
// ---------------------------------------------------------------------------

/// Check that all `w14:paraId` attribute values are < 0x80000000.
///
/// MS-OI29500 requires that `w14:paraId` values be below 0x80000000 because
/// Word treats them as signed 32-bit integers internally. Values >= 0x80000000
/// corrupt paragraph identity.
///
/// Returns findings for:
/// - **I-ANN-004**: paraId value >= 0x80000000, or value that cannot be parsed
///   as a hex u32
pub fn check_para_id_range(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            let Some(para_id) = attr_get(el, "w14:paraId") else {
                return;
            };

            match u32::from_str_radix(para_id, 16) {
                Err(_) => {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-004",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ANN-004: w14:paraId value '{para_id}' on <{}> \
                             cannot be parsed as a hex u32",
                            local_name(&el.name)
                        ),
                        location: format!("{part_path} <{}>", local_name(&el.name)),
                    });
                }
                Ok(val) if val >= 0x8000_0000 => {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-004",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ANN-004: w14:paraId value '{para_id}' (0x{val:08X}) \
                             on <{}> is >= 0x80000000; Word treats paraId as signed \
                             32-bit and values in this range corrupt paragraph identity",
                            local_name(&el.name)
                        ),
                        location: format!("{part_path} <{}>", local_name(&el.name)),
                    });
                }
                Ok(_) => {}
            }
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// I-ANN-005: Comment marker pairing
// ---------------------------------------------------------------------------

/// Check that every `commentRangeStart` has a matching `commentRangeEnd` with
/// the same `w:id` within the same story part, and vice versa. One comment id
/// may own at most one start, one end, and one reference in a story. Pairing
/// findings retain `I-ANN-005`; duplicate use is the narrower blocking
/// `I-ANN-010`, because Word tolerates lone endpoints but repairs one identity
/// reused for multiple ranges.
pub fn check_comment_marker_pairing(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        let mut starts: HashMap<String, u32> = HashMap::new();
        let mut ends: HashMap<String, u32> = HashMap::new();
        let mut references: HashMap<String, u32> = HashMap::new();

        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "commentRangeStart") {
                if let Some(id) = attr_get(el, "w:id") {
                    *starts.entry(id.clone()).or_insert(0) += 1;
                }
            } else if is_w_tag(el, "commentRangeEnd")
                && let Some(id) = attr_get(el, "w:id")
            {
                *ends.entry(id.clone()).or_insert(0) += 1;
            } else if is_w_tag(el, "commentReference")
                && let Some(id) = attr_get(el, "w:id")
            {
                *references.entry(id.clone()).or_insert(0) += 1;
            }
        });

        // Every start must have an end
        for (id, count) in &starts {
            let end_count = ends.get(id).copied().unwrap_or(0);
            if *count > 1 {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-010",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-010: commentRangeStart w:id='{id}' appears {count} times in \
                         story part '{part_path}'; one comment id cannot delimit multiple ranges"
                    ),
                    location: format!("{part_path} <commentRangeStart w:id=\"{id}\">"),
                });
            }
            if end_count == 0 {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-005",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-005: commentRangeStart w:id='{id}' has no matching \
                         commentRangeEnd in story part '{part_path}'"
                    ),
                    location: format!("{part_path} <commentRangeStart w:id=\"{id}\">"),
                });
            } else if end_count != *count {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-005",
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "I-ANN-005: commentRangeStart w:id='{id}' appears {count} time(s) \
                         but commentRangeEnd appears {end_count} time(s) in '{part_path}'"
                    ),
                    location: format!("{part_path} <commentRangeStart w:id=\"{id}\">"),
                });
            }
        }

        // Every end must have a start
        for (id, count) in &ends {
            if *count > 1 {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-010",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-010: commentRangeEnd w:id='{id}' appears {count} times in \
                         story part '{part_path}'; one comment id cannot delimit multiple ranges"
                    ),
                    location: format!("{part_path} <commentRangeEnd w:id=\"{id}\">"),
                });
            }
            if !starts.contains_key(id) {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-005",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-005: commentRangeEnd w:id='{id}' has no matching \
                         commentRangeStart in story part '{part_path}'"
                    ),
                    location: format!("{part_path} <commentRangeEnd w:id=\"{id}\">"),
                });
            }
        }

        for (id, count) in &references {
            if *count > 1 {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-010",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-ANN-010: commentReference w:id='{id}' appears {count} times in \
                         story part '{part_path}'; one comment id cannot reference multiple ranges"
                    ),
                    location: format!("{part_path} <commentReference w:id=\"{id}\">"),
                });
            }
        }
    }

    findings
}

// ---------------------------------------------------------------------------
// I-ANN-009: customXml*Range start/end pairing
// ---------------------------------------------------------------------------

/// Check that every `customXml{Ins,Del,MoveFrom,MoveTo}RangeStart` has a
/// matching `…RangeEnd` with the same `w:id` within the same story part, and
/// vice versa.
///
/// ECMA-376 §17.13.5.4–.11: these markers are start/end pairs (linked by id)
/// delimiting the revision-tracked custom-XML *markup*. A torn pair (a start
/// with no end, or an end with no start) is non-conformant. The
/// transparent-wrapper model (task #6) carries customXml/smartTag and these
/// range markers as paired `Decoration` markers, so a torn pair is now
/// constructible by an edit; this is the validator-side safety net, mirroring
/// the bookmark (I-ANN-003) and comment (I-ANN-005) pairing checks.
pub fn check_custom_xml_range_pairing(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    // The four range families, each a (startTag, endTag) pair.
    const RANGES: [(&str, &str); 4] = [
        ("customXmlInsRangeStart", "customXmlInsRangeEnd"),
        ("customXmlDelRangeStart", "customXmlDelRangeEnd"),
        ("customXmlMoveFromRangeStart", "customXmlMoveFromRangeEnd"),
        ("customXmlMoveToRangeStart", "customXmlMoveToRangeEnd"),
    ];

    let mut findings = Vec::new();

    for (part_path, root) in stories {
        for (start_tag, end_tag) in RANGES {
            let mut starts: HashMap<String, u32> = HashMap::new();
            let mut ends: HashMap<String, u32> = HashMap::new();

            walk_elements(root, &mut |el, _path| {
                if is_w_tag(el, start_tag) {
                    if let Some(id) = attr_get(el, "w:id") {
                        *starts.entry(id.clone()).or_insert(0) += 1;
                    }
                } else if is_w_tag(el, end_tag)
                    && let Some(id) = attr_get(el, "w:id")
                {
                    *ends.entry(id.clone()).or_insert(0) += 1;
                }
            });

            // Every start must have an end.
            for (id, count) in &starts {
                let end_count = ends.get(id).copied().unwrap_or(0);
                if end_count == 0 {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-009",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ANN-009: {start_tag} w:id='{id}' has no matching {end_tag} \
                             in story part '{part_path}'"
                        ),
                        location: format!("{part_path} <{start_tag} w:id=\"{id}\">"),
                    });
                } else if end_count != *count {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-009",
                        severity: ValidationSeverity::Warning,
                        message: format!(
                            "I-ANN-009: {start_tag} w:id='{id}' appears {count} time(s) but \
                             {end_tag} appears {end_count} time(s) in '{part_path}'"
                        ),
                        location: format!("{part_path} <{start_tag} w:id=\"{id}\">"),
                    });
                }
            }

            // Every end must have a start.
            for id in ends.keys() {
                if !starts.contains_key(id) {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-009",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ANN-009: {end_tag} w:id='{id}' has no matching {start_tag} \
                             in story part '{part_path}'"
                        ),
                        location: format!("{part_path} <{end_tag} w:id=\"{id}\">"),
                    });
                }
            }
        }
    }

    findings
}

// ---------------------------------------------------------------------------
// I-DOC-001 / I-DOC-002 / I-DOC-003: Document structure checks
// ---------------------------------------------------------------------------

/// Check the root element of `document.xml`:
/// - **I-DOC-001**: root must be `w:document`
/// - **I-DOC-002**: must have exactly one `w:body` child
/// - **I-DOC-003**: last element child of `w:body` must be `w:sectPr`
pub fn check_document_root(root: &Element) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // I-DOC-001: root element name and namespace
    let root_local = local_name(&root.name);
    let is_document = root_local == "document"
        && (root.prefix.as_deref() == Some("w") || root.namespace.as_deref() == Some(WML_NS));

    if !is_document {
        findings.push(ValidationFinding {
            rule_id: "I-DOC-001",
            severity: ValidationSeverity::Error,
            message: format!(
                "I-DOC-001: root element is <{root_local}> (namespace: {:?}), \
                 expected <w:document> in the WML namespace",
                root.namespace
            ),
            location: "document.xml".to_string(),
        });
        // If root isn't w:document, remaining checks are not meaningful.
        return findings;
    }

    // I-DOC-002: exactly one w:body child
    let body_children: Vec<&Element> = root
        .children
        .iter()
        .filter_map(|n| match n {
            XMLNode::Element(el) if is_w_tag(el, "body") => Some(el),
            _ => None,
        })
        .collect();

    match body_children.len() {
        0 => {
            findings.push(ValidationFinding {
                rule_id: "I-DOC-002",
                severity: ValidationSeverity::Error,
                message: "I-DOC-002: w:document has no w:body child element".to_string(),
                location: "document.xml <w:document>".to_string(),
            });
            return findings;
        }
        1 => {} // correct
        n => {
            findings.push(ValidationFinding {
                rule_id: "I-DOC-002",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-DOC-002: w:document has {n} w:body children, expected exactly 1"
                ),
                location: "document.xml <w:document>".to_string(),
            });
        }
    }

    // I-DOC-003: last element child of w:body is sectPr
    let body = body_children[0];
    let last_element_child = body.children.iter().rev().find_map(|n| match n {
        XMLNode::Element(el) => Some(el),
        _ => None,
    });

    match last_element_child {
        None => {
            findings.push(ValidationFinding {
                rule_id: "I-DOC-003",
                severity: ValidationSeverity::Error,
                message: "I-DOC-003: w:body has no element children; \
                          expected last child to be w:sectPr"
                    .to_string(),
                location: "document.xml <w:body>".to_string(),
            });
        }
        Some(el) if !is_w_tag(el, "sectPr") => {
            let el_local = local_name(&el.name);
            findings.push(ValidationFinding {
                rule_id: "I-DOC-003",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-DOC-003: last element child of w:body is <{el_local}>, \
                     expected <w:sectPr>"
                ),
                location: "document.xml <w:body>".to_string(),
            });
        }
        Some(_) => {} // correct
    }

    findings
}

// ---------------------------------------------------------------------------
// I-TC-001 + I-TC-002: Tracked change content model checks
// ---------------------------------------------------------------------------

/// Check that every native tracked-change carrier has a non-empty required
/// `w:id`. Property and table-structure changes are revision carriers too;
/// limiting this check to `w:ins` / `w:del` lets a non-conformant document pass
/// validation even though Word cannot address the change correctly.
pub fn check_required_tracked_change_ids(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            let el_local = local_name(&el.name);
            if !is_w_element(el) || !REQUIRED_TRACKED_CHANGE_ID_ELEMENTS.contains(&el_local) {
                return;
            }

            match attr_get(el, "w:id") {
                None => findings.push(ValidationFinding {
                    rule_id: "I-TC-002",
                    severity: ValidationSeverity::Error,
                    message: format!("I-TC-002: <{el_local}> is missing required w:id attribute"),
                    location: format!("{part_path} <{el_local}>"),
                }),
                Some(value) if value.is_empty() => findings.push(ValidationFinding {
                    rule_id: "I-TC-002",
                    severity: ValidationSeverity::Error,
                    message: format!("I-TC-002: <{el_local}> has empty w:id attribute"),
                    location: format!("{part_path} <{el_local}>"),
                }),
                Some(_) => {}
            }
        });
    }

    findings
}

/// Check tracked-change content model invariants in every story:
///
/// - **I-TC-001**: Every direct child element of `w:del`/`w:ins` at the
///   *paragraph level* (i.e. CT_RunTrackChange) must be an
///   `EG_ContentRunContent` member. Per ECMA-376 Annex A, these are: `w:r`,
///   `w:customXml`, `w:smartTag`, `w:sdt`, `w:dir`, `w:bdo`, plus
///   `EG_RunLevelElts` (proof/perm/range markers, nested ins/del/move).
///
///   Body-level `w:ins`/`w:del` (parent is `w:body`, `w:tc`,
///   `w:txbxContent`, etc.) uses `CT_TrackChange` which allows block
///   content (`w:p`, `w:tbl`) — these are exempt from the
///   `CT_RunTrackChange` content model check.
///
///   Additionally, paragraph-level-only elements (`w:fldSimple`,
///   `w:hyperlink`, `w:subDoc`) that belong in `EG_PContent` but NOT in
///   `EG_ContentRunContent` must not appear **anywhere** as descendants of
///   a paragraph-level tracked change, unless they are behind a content
///   boundary like `w:txbxContent`.
///
pub fn check_tracked_change_content_model(
    stories: &[(String, &Element)],
) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            let el_local = local_name(&el.name);

            if !(is_w_tag(el, "del") || is_w_tag(el, "ins")) {
                return;
            }

            // Math runs (m:r) contain tracked changes with a different
            // content model (w:rPr + m:t are valid inside w:ins/w:del in
            // math context per OOXML math extension).
            let is_math_context = path
                .iter()
                .any(|ancestor| matches!(ancestor.as_str(), "oMath" | "oMathPara"));

            // I-TC-001 (deleted-text content model): inside a w:del, run text
            // held by an opaque content control (w:sdt / w:customXml / w:smartTag)
            // must be w:delText and instruction text w:delInstrText. Left as w:t,
            // Word repairs the file and a programmatic accept-all crashes. A
            // direct <w:del><w:r><w:t> deleted run is a separately
            // tolerated Word quirk and is not flagged. Runs before the block-level
            // early return so a block-level w:del wrapping such a control is still
            // checked. Math deletions use m:t and are exempt.
            if is_w_tag(el, "del") && !is_math_context {
                let mut wrong_form = Vec::new();
                collect_deleted_text_form_violations(el, false, &mut wrong_form);
                for tag in wrong_form {
                    let expected = if tag == "t" {
                        "delText"
                    } else {
                        "delInstrText"
                    };
                    findings.push(ValidationFinding {
                        rule_id: "I-TC-001",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-TC-001: <w:{tag}> inside a content control under <w:del> must be \
                             <w:{expected}>; run text within a deletion uses the deleted-text \
                             content model"
                        ),
                        location: format!("{part_path} <w:del> // <w:{tag}>"),
                    });
                }
            }

            // Body-level w:ins/w:del uses CT_TrackChange, which allows block
            // content (w:p, w:tbl, etc.) as children. The strict
            // CT_RunTrackChange content model check only applies to
            // paragraph-level tracked changes (parent is w:p or similar).
            let is_block_level = path.last().is_some_and(|parent| {
                matches!(
                    parent.as_str(),
                    "body"
                        | "tc"
                        | "txbxContent"
                        | "sdtContent"
                        | "customXml"
                        | "ins"
                        | "del"
                        | "moveFrom"
                        | "moveTo"
                )
            });
            if is_block_level || is_math_context {
                return;
            }

            // I-TC-001: every direct child element must be in
            // EG_ContentRunContent (allowlist from the schema).
            for child in &el.children {
                let XMLNode::Element(child_el) = child else {
                    continue;
                };
                let child_local = local_name(&child_el.name);

                // WML elements: must be on the allowlist.
                if is_w_element(child_el) && !ALLOWED_IN_CT_RUN_TRACK_CHANGE.contains(&child_local)
                {
                    findings.push(ValidationFinding {
                        rule_id: "I-TC-001",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-TC-001: <w:{child_local}> is not allowed as a direct \
                                 child of <w:{el_local}>; CT_RunTrackChange only permits \
                                 EG_ContentRunContent members"
                        ),
                        location: format!("{part_path} <w:{el_local}> / <w:{child_local}>"),
                    });
                }
                // Non-WML elements (m:oMath, mc:AlternateContent, etc.)
                // are allowed by the schema extensions — don't flag them.
            }

            // I-TC-001 (descendant check): paragraph-level-only elements
            // (fldSimple, hyperlink, subDoc) must not appear anywhere inside
            // a tracked change, regardless of nesting depth. Stop at content
            // boundaries (txbxContent) which have their own scope.
            let mut forbidden = Vec::new();
            collect_forbidden_descendants(el, &mut forbidden);
            for tag in forbidden {
                findings.push(ValidationFinding {
                    rule_id: "I-TC-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-TC-001: <w:{tag}> is not allowed as a descendant \
                         of <w:{el_local}>; paragraph-level-only element \
                         found inside tracked change"
                    ),
                    location: format!("{part_path} <w:{el_local}> // <w:{tag}>"),
                });
            }
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// I-ANN-006: Footnote/endnote ID range (MS-OI29500 §2.1.300-302)
// ---------------------------------------------------------------------------

/// Check that all `w:footnote` and `w:endnote` element `w:id` attributes are
/// in the range [-2147483648, 32767].
///
/// MS-OI29500 §2.1.300-302: Word only allows footnote/endnote IDs in this
/// range. IDs above 32767 are rejected by Word even though ECMA-376 allows
/// larger values.
pub fn check_footnote_endnote_id_range(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            // Word's note-id ceiling (MS-OI29500 §2.1.300-302) applies to both the
            // note DEFINITIONS (w:footnote/w:endnote) and the in-body REFERENCES
            // (w:footnoteReference/w:endnoteReference, §17.11.14) — an out-of-range
            // reference points at a note Word could never have stored.
            let el_local = if is_w_tag(el, "footnote") {
                "footnote"
            } else if is_w_tag(el, "endnote") {
                "endnote"
            } else if is_w_tag(el, "footnoteReference") {
                "footnoteReference"
            } else if is_w_tag(el, "endnoteReference") {
                "endnoteReference"
            } else {
                return;
            };

            let Some(id_value) = attr_get(el, "w:id") else {
                return;
            };

            match id_value.parse::<i64>() {
                Err(_) => {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-006",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ANN-006: <w:{el_local}> w:id='{id_value}' \
                             cannot be parsed as an integer"
                        ),
                        location: format!("{part_path} <w:{el_local}>"),
                    });
                }
                Ok(id) if !(-2_147_483_648..=32767).contains(&id) => {
                    findings.push(ValidationFinding {
                        rule_id: "I-ANN-006",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ANN-006: <w:{el_local}> w:id='{id_value}' is outside \
                             Word's allowed range [-2147483648, 32767] \
                             (MS-OI29500 §2.1.300-302)"
                        ),
                        location: format!("{part_path} <w:{el_local}>"),
                    });
                }
                Ok(_) => {}
            }
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// I-TC-003: No nested tracked changes (MS-OI29500 §2.1.330, §2.1.334)
// ---------------------------------------------------------------------------

/// Check that `w:ins` is not nested inside another `w:ins`, and `w:del` is
/// not nested inside another `w:del`.
///
/// Cross-nesting (ins inside del, del inside ins) is valid. Only same-type
/// nesting is rejected by Word.
pub fn check_no_nested_tracked_changes(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        check_nested_tracked_inner(root, part_path, None, &mut findings);
    }

    findings
}

/// Tracks which tracked-change type is currently open (if any).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackedChangeKind {
    Ins,
    Del,
    MoveFrom,
    MoveTo,
}

fn tracked_change_kind(el: &Element) -> Option<TrackedChangeKind> {
    if is_w_tag(el, "ins") {
        Some(TrackedChangeKind::Ins)
    } else if is_w_tag(el, "del") {
        Some(TrackedChangeKind::Del)
    } else if is_w_tag(el, "moveFrom") {
        Some(TrackedChangeKind::MoveFrom)
    } else if is_w_tag(el, "moveTo") {
        Some(TrackedChangeKind::MoveTo)
    } else {
        None
    }
}

fn check_nested_tracked_inner(
    el: &Element,
    part_path: &str,
    ancestor_kind: Option<TrackedChangeKind>,
    findings: &mut Vec<ValidationFinding>,
) {
    let el_local = local_name(&el.name);
    let el_kind = tracked_change_kind(el);

    if let (Some(ancestor), Some(current)) = (ancestor_kind, el_kind)
        && ancestor == current
    {
        let tag = match current {
            TrackedChangeKind::Ins => "ins",
            TrackedChangeKind::Del => "del",
            TrackedChangeKind::MoveFrom => "moveFrom",
            TrackedChangeKind::MoveTo => "moveTo",
        };
        findings.push(ValidationFinding {
            rule_id: "I-TC-003",
            severity: ValidationSeverity::Error,
            message: format!(
                "I-TC-003: <w:{tag}> is nested inside another <w:{tag}>; \
                     Word does not support same-type nested tracked changes \
                     (MS-OI29500 §2.1.330/§2.1.334)"
            ),
            location: format!("{part_path} <w:{el_local}>"),
        });
    }

    // Pass the current element's kind to children (or preserve ancestor's if
    // this element is not a tracked-change element). A `w:txbxContent` starts
    // a separate story: a tracked change inside a textbox whose anchor run is
    // itself tracked is NOT same-type nesting (Word resolves the two stories
    // independently), so the ancestor kind resets at the boundary.
    let next_kind = if el_local == "txbxContent" {
        None
    } else {
        el_kind.or(ancestor_kind)
    };

    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            check_nested_tracked_inner(child_el, part_path, next_kind, findings);
        }
    }
}

// ---------------------------------------------------------------------------
// I-ANN-007: Comment range count limit (MS-OI29500 §2.1.315)
// ---------------------------------------------------------------------------

/// Check that the total number of `w:commentRangeStart` elements across all
/// story parts does not exceed 32767.
///
/// MS-OI29500 §2.1.315: Word will not open a file with more than 32767
/// comment ranges.
pub fn check_comment_range_count(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut count: usize = 0;

    for (_part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "commentRangeStart") {
                count += 1;
            }
        });
    }

    if count > 32767 {
        vec![ValidationFinding {
            rule_id: "I-ANN-007",
            severity: ValidationSeverity::Error,
            message: format!(
                "I-ANN-007: document contains {count} comment ranges, \
                 which exceeds Word's limit of 32767 (MS-OI29500 §2.1.315)"
            ),
            location: "word/document.xml".to_string(),
        }]
    } else {
        vec![]
    }
}

/// Returns `true` if the element is in the WML namespace (prefix `w:` or
/// namespace URI matches the WML main namespace).
fn is_w_element(el: &Element) -> bool {
    if el.prefix.as_deref() == Some("w") {
        return true;
    }
    if el.namespace.as_deref() == Some(WML_NS) {
        return true;
    }
    // Legacy: name stored as "w:foo" without parsed prefix.
    el.name.starts_with("w:")
}

// ---------------------------------------------------------------------------
// I-ANN-008: Bookmark name must not exceed 40 characters (MS-OE376 §2.13.6.2(c))
// ---------------------------------------------------------------------------

/// Check that no `w:bookmarkStart` has a `w:name` attribute longer than 40
/// characters.
///
/// Word rejects bookmark names exceeding 40 characters.
pub fn check_bookmark_name_length(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if !is_w_tag(el, "bookmarkStart") {
                return;
            }
            let Some(name) = attr_get(el, "w:name") else {
                return;
            };
            if name.len() > 40 {
                findings.push(ValidationFinding {
                    rule_id: "I-ANN-008",
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "bookmark name {:?} is {} characters long; Word rejects bookmark names > 40 characters (MS-OE376 §2.13.6.2(c))",
                        name,
                        name.len()
                    ),
                    location: part_path.clone(),
                });
            }
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// I-MATH-001 / I-MATH-002: OMML placement
// ---------------------------------------------------------------------------

/// Word rejects two math misplacements (oracle-confirmed): an `m:oMath`
/// nested directly inside another `m:oMath` (Word repairs the file), and an
/// `m:oMath` or `m:oMathPara` placed as a direct child of `w:body` or of a
/// table cell, outside any paragraph (Word cannot open the file, whether
/// the math comes before or after the cell's paragraph). Inside a paragraph,
/// in the body or in a cell, both open cleanly. Both faults are otherwise
/// schema-detectable only by deep content-model validation, so flag them
/// explicitly.
/// (ISO 29500-1 §22.1.2.77; MS-OI29500 §2.1.1687.)
pub fn check_omath_placement(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        check_omath_inner(root, part_path, false, false, &mut findings);
    }
    findings
}

fn check_omath_inner(
    el: &Element,
    part_path: &str,
    in_omath: bool,
    parent_is_body: bool,
    findings: &mut Vec<ValidationFinding>,
) {
    let is_omath = local_name(&el.name) == "oMath";
    if is_omath && in_omath {
        findings.push(ValidationFinding {
            rule_id: "I-MATH-001",
            severity: ValidationSeverity::Error,
            message: "m:oMath nested directly inside another m:oMath; Word repairs the file \
                 (ISO 29500-1 §22.1.2.77, MS-OI29500 §2.1.1687)"
                .to_string(),
            location: part_path.to_string(),
        });
    }
    if (is_omath || local_name(&el.name) == "oMathPara") && parent_is_body {
        findings.push(ValidationFinding {
            rule_id: "I-MATH-002",
            severity: ValidationSeverity::Error,
            message: format!(
                "m:{} placed as a direct child of w:body or of a table cell, outside any \
                 paragraph; Word cannot open the file (ISO 29500-1 §22.1.2.77)",
                local_name(&el.name)
            ),
            location: part_path.to_string(),
        });
    }
    let children_parent_is_body = is_w_tag(el, "body") || is_w_tag(el, "tc");
    let children_in_omath = in_omath || is_omath;
    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            check_omath_inner(
                child_el,
                part_path,
                children_in_omath,
                children_parent_is_body,
                findings,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// I-PERM-001: perm-marker id validity
// ---------------------------------------------------------------------------

/// Word accepts only a 32-bit integer for permStart/permEnd `w:id`; a
/// non-numeric id is non-conformant and Word will not load the range permission.
/// (MS-OI29500 §2.1.357/.358; ISO 29500-1 §17.13.7.1/.2.)
pub fn check_perm_id_validity(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if (is_w_tag(el, "permStart") || is_w_tag(el, "permEnd"))
                && let Some(id) = attr_get(el, "w:id")
                && id.parse::<i32>().is_err()
            {
                findings.push(ValidationFinding {
                    rule_id: "I-PERM-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "{} w:id '{id}' is not a 32-bit integer; Word will not load the \
                         range permission (MS-OI29500 §2.1.357/.358, ISO 29500-1 §17.13.7)",
                        local_name(&el.name)
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-TC-004: moved-from text is plain text
// ---------------------------------------------------------------------------

/// `w:moveFrom` content is runs of plain text (ECMA-376 §17.13.5.22); deleted
/// text (`w:delText`, `w:delInstrText`) belongs only inside `w:del`. Word
/// repairs a file whose moveFrom runs carry deleted text (checked in Word: it
/// wraps each such run in a new `w:del` by "Unknown" and the revisions are
/// lost). The same holds for a field instruction: Word 16 writes a moved
/// field's instruction as `w:instrText` and reports `w:delInstrText` inside
/// `w:moveFrom` as corrupted (observed). A `w:del` nested in the moveFrom is
/// its own scope and is not reported here.
pub fn check_move_from_text_form(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if !(is_w_tag(el, "delText") || is_w_tag(el, "delInstrText")) {
                return;
            }
            let scope = path.iter().rev().find(|ancestor| {
                matches!(
                    ancestor.as_str(),
                    "del" | "ins" | "moveFrom" | "moveTo" | "txbxContent"
                )
            });
            if scope.map(String::as_str) == Some("moveFrom") {
                let text: String = el
                    .children
                    .iter()
                    .filter_map(|child| match child {
                        XMLNode::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
                    .chars()
                    .take(40)
                    .collect();
                findings.push(ValidationFinding {
                    rule_id: "I-TC-004",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-TC-004: {part_path}: a run inside w:moveFrom spells its text as \
                         w:{} ({text:?}); moved-from text is written as w:t (ECMA-376 \
                         §17.13.5.22). Word reports the file as corrupted and repairs it",
                        local_name(&el.name)
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-TC-005: inserted text is plain text, deleted text is deleted text
// ---------------------------------------------------------------------------

/// Deleted text is written as `w:delText`, all other text as `w:t`
/// (ECMA-376 §17.13.5.14, §17.13.5.18). Every case here was checked against
/// Word, which repairs a document that spells the text the other way.
///
/// Inside a paragraph, any enclosing `w:del` makes the text deleted, in
/// either nesting order: `w:delText` opens cleanly inside a deletion nested
/// in an insertion and inside an insertion nested in a deletion, and `w:t`
/// is repaired in both. With no `w:del`, inside a `w:ins` or outside any
/// tracked change, the text is `w:t`.
///
/// A `w:del` that wraps a whole paragraph is outside the schema, but Word
/// reads it: the paragraph's text is `w:delText`, unless the paragraph mark
/// is itself marked deleted (`w:pPr/w:rPr/w:del`), in which case it is
/// `w:t`. A `w:ins` that wraps a whole paragraph leaves the text `w:t`.
///
/// A paragraph that sits under a block-level change and also holds tracked
/// changes of its own was not checked and is not judged. A text box starts
/// a new scope. `w:moveFrom` is I-TC-004.
pub fn check_tracked_text_form(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        let mut problems = Vec::new();
        tracked_text_form_in(root, TrackedTextScope::default(), &mut problems);
        for (place, found, expected) in problems {
            findings.push(ValidationFinding {
                rule_id: "I-TC-005",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-TC-005: {part_path}: a run {place} spells its text as w:{found}; text \
                     there is written as {expected}. Word reports the file as corrupted and \
                     repairs it"
                ),
                location: part_path.to_string(),
            });
        }
    }
    findings
}

/// Where a run's text sits, for [`check_tracked_text_form`].
#[derive(Clone, Copy, Default)]
struct TrackedTextScope {
    /// A `w:del` / `w:ins` wraps the enclosing paragraph.
    block_del: bool,
    block_ins: bool,
    in_paragraph: bool,
    /// The enclosing paragraph's mark is marked deleted.
    mark_deleted: bool,
    /// Tracked changes between the run and its paragraph.
    inline_del: bool,
    inline_ins: bool,
    moved: bool,
}

fn paragraph_mark_is_deleted(paragraph: &Element) -> bool {
    element_children(paragraph)
        .filter(|child| is_w_tag(child, "pPr"))
        .flat_map(element_children)
        .filter(|child| is_w_tag(child, "rPr"))
        .flat_map(element_children)
        .any(|child| is_w_tag(child, "del"))
}

fn tracked_text_form_in(
    el: &Element,
    scope: TrackedTextScope,
    problems: &mut Vec<(&'static str, String, &'static str)>,
) {
    let deleted_form = is_w_tag(el, "delText") || is_w_tag(el, "delInstrText");
    if (deleted_form || is_w_tag(el, "t")) && scope.in_paragraph && !scope.moved {
        let block_level = scope.block_del || scope.block_ins;
        let inline = scope.inline_del || scope.inline_ins;
        // (place, the text must be the deleted form)
        let expected = if block_level && inline {
            None
        } else if scope.inline_del {
            Some(("inside w:del", true))
        } else if scope.inline_ins {
            Some(("inside w:ins", false))
        } else if scope.block_del && scope.mark_deleted {
            Some(("in a deleted paragraph whose mark is deleted", false))
        } else if scope.block_del {
            Some(("in a paragraph inside w:del", true))
        } else if scope.block_ins {
            Some(("in a paragraph inside w:ins", false))
        } else {
            Some(("outside any tracked change", false))
        };
        if let Some((place, must_be_deleted_form)) = expected
            && must_be_deleted_form != deleted_form
        {
            problems.push((
                place,
                local_name(&el.name).to_string(),
                if must_be_deleted_form {
                    "w:delText"
                } else {
                    "w:t"
                },
            ));
        }
        return;
    }
    let mut inner = scope;
    if is_w_tag(el, "txbxContent") {
        inner = TrackedTextScope::default();
    } else if is_w_tag(el, "p") {
        inner = TrackedTextScope {
            block_del: scope.block_del,
            block_ins: scope.block_ins,
            in_paragraph: true,
            mark_deleted: paragraph_mark_is_deleted(el),
            ..TrackedTextScope::default()
        };
    } else if is_w_tag(el, "del") {
        if scope.in_paragraph {
            inner.inline_del = true;
        } else {
            inner.block_del = true;
        }
    } else if is_w_tag(el, "ins") {
        if scope.in_paragraph {
            inner.inline_ins = true;
        } else {
            inner.block_ins = true;
        }
    } else if is_w_tag(el, "moveFrom") || is_w_tag(el, "moveTo") {
        inner.moved = true;
    } else if is_w_tag(el, "tbl") {
        // A table inside a block-level change was not checked.
        inner = TrackedTextScope::default();
    }
    for child in element_children(el) {
        tracked_text_form_in(child, inner, problems);
    }
}

// ---------------------------------------------------------------------------
// I-DOC-004: a run sits in a paragraph, never directly in the body
// ---------------------------------------------------------------------------

/// `w:body` holds block-level content; a `w:r` belongs inside a paragraph
/// (ECMA-376 §17.2.2). Checked against Word: it cannot open a document with
/// a run, or a `w:t`, placed directly in the body, even with repair.
pub fn check_run_directly_in_body(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if (is_w_tag(el, "r") || is_w_tag(el, "t"))
                && path.last().map(String::as_str) == Some("body")
            {
                findings.push(ValidationFinding {
                    rule_id: "I-DOC-004",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-DOC-004: {part_path}: a w:{} sits directly in w:body, outside any \
                         paragraph. Word cannot open such a file",
                        local_name(&el.name)
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-STORY-002: a run sits in a paragraph inside a note, comment, header or footer
// ---------------------------------------------------------------------------

/// A comment, footnote, endnote, header or footer holds block-level content,
/// like the body. Checked against Word with the same story with and without
/// the paragraph around its runs: a `w:r` directly in `w:comment`,
/// `w:footnote` or `w:endnote` makes Word repair the file; directly in
/// `w:hdr` or `w:ftr`, Word cannot open it.
pub fn check_run_directly_in_story(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if !is_w_tag(el, "r") {
                return;
            }
            let outcome = match path.last().map(String::as_str) {
                Some("comment" | "footnote" | "endnote") => "Word repairs such a file",
                Some("hdr" | "ftr") => "Word cannot open such a file",
                _ => return,
            };
            findings.push(ValidationFinding {
                rule_id: "I-STORY-002",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-STORY-002: {part_path}: a w:r sits directly in w:{}, outside any \
                     paragraph. {outcome}",
                    path.last().map(String::as_str).unwrap_or_default()
                ),
                location: part_path.to_string(),
            });
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-ATTR-001: properties whose value Word requires carry it
// ---------------------------------------------------------------------------

/// Property elements Word refuses without their `w:val`, as
/// `(element, parent, Word's outcome)`.
///
/// Checked against Word with one document carrying all of them, then the same
/// document with the `w:val` of one removed: without it Word cannot open the
/// file for `pStyle`, `jc` and `outlineLvl` in a paragraph's properties,
/// `numId` and `ilvl` in its numbering, `rStyle` in a run's properties and
/// `tblStyle` in a table's; it repairs the file for `highlight` and
/// `vertAlign`. The same removal from `w:sz`, `w:color`, `w:lang` and a tab
/// stop's `w:tab` opens cleanly, so those are not listed.
const VALUE_REQUIRED: &[(&str, &str, &str)] = &[
    ("pStyle", "pPr", "Word cannot open such a file"),
    ("jc", "pPr", "Word cannot open such a file"),
    ("outlineLvl", "pPr", "Word cannot open such a file"),
    ("numId", "numPr", "Word cannot open such a file"),
    ("ilvl", "numPr", "Word cannot open such a file"),
    ("rStyle", "rPr", "Word cannot open such a file"),
    ("tblStyle", "tblPr", "Word cannot open such a file"),
    ("highlight", "rPr", "Word repairs such a file"),
    ("vertAlign", "rPr", "Word repairs such a file"),
];

// ---------------------------------------------------------------------------
// I-ATTR-002: enumerated property values are values Word knows
// ---------------------------------------------------------------------------

const ST_JC: &[&str] = &[
    "start",
    "center",
    "end",
    "both",
    "mediumKashida",
    "distribute",
    "numTab",
    "highKashida",
    "lowKashida",
    "thaiDistribute",
    "left",
    "right",
];
const ST_HIGHLIGHT_COLOR: &[&str] = &[
    "black",
    "blue",
    "cyan",
    "green",
    "magenta",
    "red",
    "yellow",
    "white",
    "darkBlue",
    "darkCyan",
    "darkGreen",
    "darkMagenta",
    "darkRed",
    "darkYellow",
    "darkGray",
    "lightGray",
    "none",
];
const ST_VERTICAL_ALIGN_RUN: &[&str] = &["baseline", "superscript", "subscript"];

const ST_TEXT_ALIGNMENT: &[&str] = &["top", "center", "baseline", "bottom", "auto"];
const ST_TEXT_DIRECTION: &[&str] = &[
    "tb", "rl", "lr", "tbV", "rlV", "lrV", "btLr", "lrTb", "lrTbV", "tbLrV", "tbRl", "tbRlV",
];
const ST_TEXT_EFFECT: &[&str] = &[
    "blinkBackground",
    "lights",
    "antsBlack",
    "antsRed",
    "shimmer",
    "sparkle",
    "none",
];
const ST_EM: &[&str] = &["none", "dot", "comma", "circle", "underDot"];
const ST_VERTICAL_JC: &[&str] = &["top", "center", "both", "bottom"];

const CANNOT_OPEN: &str = "Word cannot open such a file";
const REPAIRS: &str = "Word repairs such a file";

/// What a property's value must be, for the properties whose wrong value
/// Word was seen to refuse or repair.
enum ValueRule {
    /// One of these tokens.
    OneOf(&'static [&'static str]),
    /// A decimal integer.
    Integer,
    /// A decimal integer from 0 to 9; a number outside repairs, anything
    /// else cannot open.
    OutlineLevel,
}

/// `(element, parent, attribute, rule, Word's outcome)`. Each row was
/// checked against Word with one document whose value of that one property
/// was replaced (`bogus`, a word where a number goes, or the named value),
/// against the same document with a valid value:
///
/// | property | value tried | Word |
/// |---|---|---|
/// | pPr `jc` | `justify`, `bogus` | cannot open |
/// | pPr `outlineLvl` | `x` / `10` | cannot open / repairs |
/// | pPr `textAlignment` | `bogus` | cannot open |
/// | pPr and sectPr `textDirection` | `vertical` | cannot open |
/// | rPr `highlight`, `vertAlign` | `bogus` | repairs |
/// | rPr `effect`, `em` | `bogus` | repairs |
/// | tcPr `gridSpan` | `two` | repairs |
/// | tcPr `vAlign` | `bogus` | cannot open |
/// | sectPr `cols/@w:num` | `two` | cannot open |
/// | sectPr `vAlign` | `bogus` | cannot open |
///
/// The same change opened cleanly (and is not judged) for: pPr `spacing/@w:lineRule`,
/// `pBdr` edge `w:val`, `tab/@w:val`; rPr `u`, `color` (`red`), `lang`,
/// `rFonts/@w:hint`, `sz`, `kern`, `spacing` (words), `shd`, `b`; tblPr
/// `tblLayout`, `jc`, `tblW/@w:type`; tcPr `vMerge`; trPr `trHeight/@w:hRule`;
/// sectPr `pgSz/@w:orient`, `pgNumType/@w:fmt`; `fldChar/@w:fldCharType`.
/// sectPr `type` is not judged: `bogus` cannot open but `page` (outside
/// ST_SectionMark too) opens cleanly, so the values Word takes are not
/// established.
const VALUE_RULES: &[(&str, &str, &str, ValueRule, &str)] = &[
    ("jc", "pPr", "val", ValueRule::OneOf(ST_JC), CANNOT_OPEN),
    (
        "outlineLvl",
        "pPr",
        "val",
        ValueRule::OutlineLevel,
        CANNOT_OPEN,
    ),
    (
        "textAlignment",
        "pPr",
        "val",
        ValueRule::OneOf(ST_TEXT_ALIGNMENT),
        CANNOT_OPEN,
    ),
    (
        "textDirection",
        "pPr",
        "val",
        ValueRule::OneOf(ST_TEXT_DIRECTION),
        CANNOT_OPEN,
    ),
    (
        "textDirection",
        "sectPr",
        "val",
        ValueRule::OneOf(ST_TEXT_DIRECTION),
        CANNOT_OPEN,
    ),
    (
        "highlight",
        "rPr",
        "val",
        ValueRule::OneOf(ST_HIGHLIGHT_COLOR),
        REPAIRS,
    ),
    (
        "vertAlign",
        "rPr",
        "val",
        ValueRule::OneOf(ST_VERTICAL_ALIGN_RUN),
        REPAIRS,
    ),
    (
        "effect",
        "rPr",
        "val",
        ValueRule::OneOf(ST_TEXT_EFFECT),
        REPAIRS,
    ),
    ("em", "rPr", "val", ValueRule::OneOf(ST_EM), REPAIRS),
    ("gridSpan", "tcPr", "val", ValueRule::Integer, REPAIRS),
    (
        "vAlign",
        "tcPr",
        "val",
        ValueRule::OneOf(ST_VERTICAL_JC),
        CANNOT_OPEN,
    ),
    ("cols", "sectPr", "num", ValueRule::Integer, CANNOT_OPEN),
    (
        "vAlign",
        "sectPr",
        "val",
        ValueRule::OneOf(ST_VERTICAL_JC),
        CANNOT_OPEN,
    ),
];

/// I-ATTR-002: property values outside their type, per [`VALUE_RULES`]. A
/// missing value is I-ATTR-001's.
pub fn check_property_value_enumerations(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            let parent = path.last().map(String::as_str);
            for (name, expected_parent, attribute, rule, outcome) in VALUE_RULES {
                if !is_w_tag(el, name) || parent != Some(*expected_parent) {
                    continue;
                }
                let Some(value) = el
                    .attributes
                    .iter()
                    .find(|(attr, _)| attr.local_name == *attribute)
                    .map(|(_, value)| value.as_str())
                else {
                    continue;
                };
                let outcome = match rule {
                    ValueRule::OneOf(allowed) => (!allowed.contains(&value)).then_some(*outcome),
                    ValueRule::Integer => value.parse::<i64>().is_err().then_some(*outcome),
                    ValueRule::OutlineLevel => match value.parse::<i64>() {
                        Err(_) => Some(CANNOT_OPEN),
                        Ok(level) if level > 9 => Some(REPAIRS),
                        Ok(_) => None,
                    },
                };
                if let Some(outcome) = outcome {
                    findings.push(ValidationFinding {
                        rule_id: "I-ATTR-002",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-ATTR-002: {part_path}: w:{name}/@w:{attribute} {value:?} in w:{expected_parent} \
                             is not a value Word knows for it. {outcome}"
                        ),
                        location: part_path.to_string(),
                    });
                }
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-SDT-004: what a plain-text content control holds
// ---------------------------------------------------------------------------

/// Whether a content control shows its placeholder (`w:showingPlcHdr` with a
/// `w:placeholder`). Word opens such a control cleanly where it repairs the
/// same control showing its own content: a picture control naming a
/// drop-down list as well, and a drop-down list around two paragraphs, are
/// clean when they show their placeholder (observed).
pub(crate) fn sdt_shows_placeholder(sdt_pr: &Element) -> bool {
    element_children(sdt_pr).any(|child| is_w_tag(child, "showingPlcHdr"))
        && element_children(sdt_pr).any(|child| is_w_tag(child, "placeholder"))
}

/// Checked against Word, one plain-text content control (`w:text`) with
/// and without one piece of content (the same control without `w:text`,
/// rich text, opens cleanly in each case):
///
/// - a field inside it (`w:fldChar`/`w:instrText` or a `w:fldSimple`):
///   repaired;
/// - a `w:bookmarkEnd` inside it whose `w:bookmarkStart` is outside: repaired;
///   a whole bookmark, or only its start, inside an inline control: clean;
/// - in a control around paragraphs (block level), a `w:bookmarkEnd` after
///   a run in one of its paragraphs (its start inside or outside):
///   repaired. A bookmark end before the paragraph's runs, a bookmark start
///   anywhere, and a bookmark end standing beside the paragraphs, directly
///   in the control, open cleanly.
pub fn check_plain_text_control_content(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    fn collect<'a>(element: &'a Element, out: &mut Vec<&'a Element>) {
        for child in element_children(element) {
            out.push(child);
            collect(child, out);
        }
    }
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if !is_w_tag(el, "sdt") {
                return;
            }
            let Some(sdt_pr) = element_children(el).find(|child| is_w_tag(child, "sdtPr")) else {
                return;
            };
            // A control showing its placeholder is not judged: one around
            // table cells holding a field, showing its placeholder, opens
            // cleanly (observed).
            if !element_children(sdt_pr).any(|child| is_w_tag(child, "text"))
                || sdt_shows_placeholder(sdt_pr)
            {
                return;
            }
            let Some(content) = element_children(el).find(|child| is_w_tag(child, "sdtContent"))
            else {
                return;
            };
            let mut inside = Vec::new();
            collect(content, &mut inside);
            let block = path.last().map(String::as_str) != Some("p");
            let id = |element: &Element| attr_get(element, "w:id").cloned();
            let starts: Vec<Option<String>> = inside
                .iter()
                .filter(|e| is_w_tag(e, "bookmarkStart"))
                .map(|e| id(e))
                .collect();
            let problem = if inside.iter().any(|e| {
                is_w_tag(e, "fldChar") || is_w_tag(e, "instrText") || is_w_tag(e, "fldSimple")
            }) {
                Some("holds a field")
            } else if block
                && element_children(content)
                    .filter(|c| is_w_tag(c, "p"))
                    .any(|paragraph| {
                        element_children(paragraph)
                            .skip_while(|child| !is_w_tag(child, "r"))
                            .any(|later| is_w_tag(later, "bookmarkEnd"))
                    })
            {
                Some("holds a paragraph with a bookmark end after its text")
            } else if !block
                && inside
                    .iter()
                    .any(|e| is_w_tag(e, "bookmarkEnd") && !starts.contains(&id(e)))
            {
                Some("holds the end of a bookmark that starts outside it")
            } else {
                None
            };
            if let Some(problem) = problem {
                findings.push(ValidationFinding {
                    rule_id: "I-SDT-004",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-SDT-004: {part_path}: a plain-text content control {problem}. Word \
                         repairs such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-STRUCT-001: structural faults Word refuses or repairs in a story
// ---------------------------------------------------------------------------

const ST_REL_FROM_H: &[&str] = &[
    "margin",
    "page",
    "column",
    "character",
    "leftMargin",
    "rightMargin",
    "insideMargin",
    "outsideMargin",
];
const ST_REL_FROM_V: &[&str] = &[
    "margin",
    "page",
    "paragraph",
    "line",
    "topMargin",
    "bottomMargin",
    "insideMargin",
    "outsideMargin",
];
const ST_ALIGN_H: &[&str] = &["left", "right", "center", "inside", "outside"];
const ST_ALIGN_V: &[&str] = &["top", "bottom", "center", "inside", "outside"];
const MATH_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// Each checked against Word with one document with and without the fault:
///
/// - a drawing's `wp:extent` with a negative or fractional `cx`/`cy`, a
///   `wp:posOffset` that is not an integer (`.75` added, or a word), a
///   `wp:positionH`/`wp:positionV` `relativeFrom` outside its type, or a
///   `wp:align` outside its type (`middle`): repaired. A negative
///   `wp:posOffset` opens cleanly;
/// - a `w:listItem` without `w:value`: cannot open;
/// - an `mc:AlternateContent` whose `mc:Fallback` comes before its
///   `mc:Choice`: repaired;
/// - a `w:bookmarkStart` without `w:name` whose `w:bookmarkEnd` is in the
///   part: repaired (one displaced by custom XML opens cleanly);
/// - an inline content control (in a paragraph) whose content holds a
///   paragraph, followed in that paragraph by a run: cannot open (with only
///   bookmark ends or nothing after it, clean);
/// - an `m:t` outside an `m:r`: cannot open (inside a tracked change that is
///   inside the `m:r`, clean).
pub fn check_story_structure(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        let mut bookmark_end_ids = std::collections::HashSet::new();
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "bookmarkEnd")
                && let Some(id) = attr_get(el, "w:id")
            {
                bookmark_end_ids.insert(id.clone());
            }
        });
        walk_elements(root, &mut |el, path| {
            let parent = path.last().map(String::as_str);
            let attr = |name: &str| {
                el.attributes
                    .iter()
                    .find(|(attr, _)| attr.local_name == name)
                    .map(|(_, value)| value.as_str())
            };
            let in_wp = el.namespace.as_deref() == Some(WORDPROCESSING_DRAWING_NS);
            let local = local_name(&el.name);
            let text = || {
                el.get_text()
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default()
            };
            let problem: Option<(String, &str)> = if in_wp && local == "extent" {
                ["cx", "cy"].iter().find_map(|name| {
                    let value = attr(name)?;
                    value
                        .parse::<u64>()
                        .is_err()
                        .then(|| (format!("wp:extent/@{name} is {value:?}"), REPAIRS))
                })
            } else if in_wp && local == "posOffset" {
                let value = text();
                value
                    .parse::<i64>()
                    .is_err()
                    .then(|| (format!("wp:posOffset is {value:?}"), REPAIRS))
            } else if in_wp && (local == "positionH" || local == "positionV") {
                let allowed = if local == "positionH" {
                    ST_REL_FROM_H
                } else {
                    ST_REL_FROM_V
                };
                attr("relativeFrom")
                    .filter(|value| !allowed.contains(value))
                    .map(|value| (format!("wp:{local}/@relativeFrom is {value:?}"), REPAIRS))
            } else if in_wp && local == "align" && matches!(parent, Some("positionH" | "positionV"))
            {
                let allowed = if parent == Some("positionH") {
                    ST_ALIGN_H
                } else {
                    ST_ALIGN_V
                };
                let value = text();
                (!allowed.contains(&value.as_str()))
                    .then(|| (format!("wp:align is {value:?}"), REPAIRS))
            } else if is_w_tag(el, "listItem") && attr("value").is_none() {
                Some(("a w:listItem has no w:value".to_string(), CANNOT_OPEN))
            } else if el.namespace.as_deref() == Some(MC_NS) && local == "AlternateContent" {
                let order: Vec<&str> = element_children(el)
                    .filter(|child| child.namespace.as_deref() == Some(MC_NS))
                    .map(|child| local_name(&child.name))
                    .collect();
                let fallback = order.iter().position(|name| *name == "Fallback");
                let last_choice = order.iter().rposition(|name| *name == "Choice");
                matches!((fallback, last_choice), (Some(f), Some(c)) if f < c).then(|| {
                    (
                        "an mc:Fallback comes before an mc:Choice".to_string(),
                        REPAIRS,
                    )
                })
            } else if is_w_tag(el, "bookmarkStart")
                && attr("name").is_none()
                && attr("displacedByCustomXml").is_none()
                && attr("id").is_some_and(|id| bookmark_end_ids.contains(id))
            {
                Some(("a w:bookmarkStart has no w:name".to_string(), REPAIRS))
            } else if is_w_tag(el, "p")
                && element_children(el)
                    .skip_while(|child| {
                        !(is_w_tag(child, "sdt")
                            && element_children(child)
                                .filter(|c| is_w_tag(c, "sdtContent"))
                                .flat_map(element_children)
                                .any(|c| is_w_tag(c, "p")))
                    })
                    .skip(1)
                    .any(|later| is_w_tag(later, "r"))
            {
                Some((
                    "an inline content control holds a paragraph and a run follows it".to_string(),
                    CANNOT_OPEN,
                ))
            } else if el.namespace.as_deref() == Some(MATH_NS)
                && local == "t"
                && path
                    .iter()
                    .rev()
                    .find(|ancestor| {
                        !matches!(ancestor.as_str(), "ins" | "del" | "moveFrom" | "moveTo")
                    })
                    .map(String::as_str)
                    != Some("r")
            {
                Some(("an m:t sits outside an m:r".to_string(), CANNOT_OPEN))
            } else {
                None
            };
            if let Some((what, outcome)) = problem {
                findings.push(ValidationFinding {
                    rule_id: "I-STRUCT-001",
                    severity: ValidationSeverity::Error,
                    message: format!("I-STRUCT-001: {part_path}: {what}. {outcome}"),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-NS-003: elements in a namespace Word does not know
// ---------------------------------------------------------------------------

/// Namespace URIs whose elements Word reads or skips by itself: the OOXML
/// and Microsoft Office families. An element in any other namespace is one
/// only `mc:Ignorable` makes Word skip.
fn is_office_namespace(uri: &str) -> bool {
    [
        "http://schemas.openxmlformats.org/",
        "http://purl.oclc.org/ooxml/",
        "http://schemas.microsoft.com/",
        "urn:schemas-microsoft-com:",
        "http://www.w3.org/",
    ]
    .iter()
    .any(|prefix| uri.starts_with(prefix))
}

/// I-NS-003. Checked against Word: an element in a namespace outside the
/// Office families (`urn:example:…`, or a default-namespace element), not
/// made ignorable by `mc:Ignorable`, in a body paragraph, at body level, in
/// a run, in run properties, or in a header paragraph: Word cannot open
/// the file. The same element made ignorable opens cleanly. Content inside
/// an `mc:AlternateContent` is left to its branch selection and not judged.
pub fn check_unknown_namespace_elements(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    fn walk(
        part_path: &str,
        element: &Element,
        ignorable: &mut Vec<String>,
        findings: &mut Vec<ValidationFinding>,
    ) {
        let pushed = element
            .attributes
            .iter()
            .find(|(name, _)| {
                name.local_name == "Ignorable" && name.namespace.as_deref() == Some(MC_NS)
            })
            .map(|(_, value)| {
                value
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let count = pushed.len();
        ignorable.extend(pushed);
        let namespace = element.namespace.as_deref().unwrap_or("");
        let skipped = element
            .prefix
            .as_deref()
            .is_some_and(|prefix| ignorable.iter().any(|p| p == prefix));
        if !skipped && !namespace.is_empty() && !is_office_namespace(namespace) {
            findings.push(ValidationFinding {
                rule_id: "I-NS-003",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-NS-003: {part_path}: element {:?} is in namespace {namespace:?}, which \
                     Word does not know, and no mc:Ignorable names it. Word cannot open such \
                     a file",
                    element.name
                ),
                location: part_path.to_string(),
            });
        } else if !skipped
            && !(element.namespace.as_deref() == Some(MC_NS) && element.name == "AlternateContent")
        {
            for child in element_children(element) {
                walk(part_path, child, ignorable, findings);
            }
        }
        ignorable.truncate(ignorable.len() - count);
    }
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk(part_path, root, &mut Vec::new(), &mut findings);
    }
    findings
}

// ---------------------------------------------------------------------------
// I-RUN-003: no run or paragraph out of place inside w:t
// ---------------------------------------------------------------------------

/// `w:t` holds text only. Checked against Word: a `w:r` inside a `w:t`
/// makes Word refuse the file; so does text after a `w:p` held in a `w:t`,
/// and a run following, in the same paragraph, the run whose `w:t` holds a
/// `w:p`. A `w:t` holding paragraphs and nothing after them, as the last
/// content of its paragraph, opens cleanly (and is read as Word reads it).
pub fn check_text_element_content(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    let mut report = |part_path: &str, what: &str| {
        findings.push(ValidationFinding {
            rule_id: "I-RUN-003",
            severity: ValidationSeverity::Error,
            message: format!("I-RUN-003: {part_path}: {what}. Word cannot open such a file"),
            location: part_path.to_string(),
        });
    };
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "t") {
                if element_children(el).any(|child| is_w_tag(child, "r")) {
                    report(part_path, "a w:r sits inside a w:t");
                }
                let mut after_paragraph = false;
                for child in &el.children {
                    match child {
                        XMLNode::Element(child) if is_w_tag(child, "p") => after_paragraph = true,
                        XMLNode::Text(text) | XMLNode::CData(text)
                            if after_paragraph && !text.trim().is_empty() =>
                        {
                            report(part_path, "text follows a w:p held inside a w:t");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            if is_w_tag(el, "p") {
                let runs: Vec<&Element> =
                    element_children(el).filter(|c| is_w_tag(c, "r")).collect();
                let holds_paragraph = |run: &Element| {
                    element_children(run).any(|t| {
                        is_w_tag(t, "t") && element_children(t).any(|inner| is_w_tag(inner, "p"))
                    })
                };
                if let Some(position) = runs.iter().position(|run| holds_paragraph(run))
                    && position + 1 < runs.len()
                {
                    report(
                        part_path,
                        "a run follows the run whose w:t holds a w:p, in the same paragraph",
                    );
                }
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-DRAW-003: a picture's graphic data names the picture type
// ---------------------------------------------------------------------------

const PICTURE_URI: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const GROUP_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup";

/// `a:graphicData/@uri` names the kind of graphic it holds. Checked against
/// Word with an inline picture whose graphic data names another kind: with
/// `.../drawingml/2006/pictures` Word repairs the file, with the chart uri
/// it cannot open it; with the picture uri it opens cleanly. A shape group
/// (`wpg:wgp`) whose graphic data names `.../2006/pictures` instead of the
/// group namespace is repaired too (a Word-clean document changed in that
/// one value). Only graphic data holding a `pic:pic` or a `wpg:wgp` is
/// judged: a lone shape under the group uri opens cleanly.
pub fn check_picture_graphic_data_uri(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if el.namespace.as_deref() != Some(DRAWING_NS) || local_name(&el.name) != "graphicData"
            {
                return;
            }
            let expected = element_children(el).find_map(|child| {
                match (child.namespace.as_deref(), local_name(&child.name)) {
                    (Some(PICTURE_NS), "pic") => Some(("a picture", PICTURE_URI)),
                    (Some(GROUP_NS), "wgp") => Some(("a shape group", GROUP_NS)),
                    _ => None,
                }
            });
            let uri = el
                .attributes
                .iter()
                .find(|(attr, _)| attr.local_name == "uri")
                .map(|(_, value)| value.as_str());
            if let Some((kind, expected)) = expected
                && let Some(uri) = uri
                && uri != expected
            {
                findings.push(ValidationFinding {
                    rule_id: "I-DRAW-003",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-DRAW-003: {part_path}: graphic data holding {kind} names {uri:?}, \
                         not {expected:?}. Word repairs such a file or cannot open it"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

pub fn check_required_property_values(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            let Some((name, parent, outcome)) = VALUE_REQUIRED
                .iter()
                .find(|(name, _, _)| is_w_tag(el, name))
            else {
                return;
            };
            if path.last().map(String::as_str) != Some(*parent)
                || el
                    .attributes
                    .iter()
                    .any(|(attr, _)| attr.local_name == "val")
            {
                return;
            }
            findings.push(ValidationFinding {
                rule_id: "I-ATTR-001",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-ATTR-001: {part_path}: a w:{name} in w:{parent} has no w:val. {outcome}"
                ),
                location: part_path.to_string(),
            });
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-FLD-001: a simple field carries its instruction
// ---------------------------------------------------------------------------

/// `w:fldSimple/@w:instr` is required (ECMA-376 §17.16.19). Checked against
/// Word: without the attribute it cannot open the file; with an empty
/// instruction (`w:instr=""`) the file opens cleanly.
pub fn check_simple_field_instruction(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "fldSimple")
                && !el
                    .attributes
                    .iter()
                    .any(|(attr, _)| attr.local_name == "instr")
            {
                findings.push(ValidationFinding {
                    rule_id: "I-FLD-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-FLD-001: {part_path}: a w:fldSimple has no w:instr. Word cannot \
                         open such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-SDT-003: a picture content control is no other kind as well
// ---------------------------------------------------------------------------

/// `w:sdtPr` names at most one kind of control (ECMA-376 §17.5.2.38, a
/// choice), yet Word tolerates most doubles. Checked against Word, one
/// content control with two kind elements: `w:picture` together with any
/// other kind — `w:text`, `w:richText`, `w:date`, `w:dropDownList`,
/// `w:comboBox`, `w:docPartObj`, `w:docPartList`, `w:equation`,
/// `w:citation`, `w:group`, `w:bibliography`, `w14:checkbox` or a second
/// `w:picture`, in either order — makes Word repair the file. Without a
/// picture, two kinds open cleanly (two `w:text`, `w:richText` with
/// `w:text`, `w:date` with `w:dropDownList`, `w:comboBox` with
/// `w:dropDownList`, `w14:checkbox` with `w:text`, and `w:docPartObj`,
/// `w:citation`, `w:equation`, `w:group` or `w:bibliography` with `w:text`).
const CONTROL_KINDS: &[&str] = &[
    "picture",
    "text",
    "richText",
    "date",
    "dropDownList",
    "comboBox",
    "docPartObj",
    "docPartList",
    "equation",
    "citation",
    "group",
    "bibliography",
];

const WORD_2010_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";

/// The kind a child of `w:sdtPr` names, if it names one.
fn control_kind(child: &Element) -> Option<&str> {
    if local_name(&child.name) == "checkbox"
        && (child.namespace.as_deref() == Some(WORD_2010_NS)
            || child.prefix.as_deref() == Some("w14"))
    {
        return Some("w14:checkbox");
    }
    CONTROL_KINDS
        .iter()
        .find(|kind| is_w_tag(child, kind))
        .map(|_| local_name(&child.name))
}

pub fn check_picture_control_kind(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if !is_w_tag(el, "sdtPr") {
                return;
            }
            let kinds: Vec<&str> = element_children(el).filter_map(control_kind).collect();
            if kinds.contains(&"picture") && kinds.len() > 1 && !sdt_shows_placeholder(el) {
                findings.push(ValidationFinding {
                    rule_id: "I-SDT-003",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-SDT-003: {part_path}: a picture content control names a second \
                         kind of control ({}). Word repairs such a file",
                        kinds.join(", ")
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-DOC-005: text sits in a run, never directly in a paragraph
// ---------------------------------------------------------------------------

/// `w:t` belongs inside a run (ECMA-376 §17.3.3.31). Checked against Word:
/// it cannot open a document with a `w:t` placed directly in a paragraph,
/// even with repair.
pub fn check_text_directly_in_paragraph(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if is_w_tag(el, "t") && path.last().map(String::as_str) == Some("p") {
                findings.push(ValidationFinding {
                    rule_id: "I-DOC-005",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-DOC-005: {part_path}: a w:t sits directly in w:p, outside any run. \
                         Word cannot open such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-RUN-001: a bidirectional override or embedding states its direction
// ---------------------------------------------------------------------------

/// `w:bdo` and `w:dir` carry their direction in `w:val` (ECMA-376 §17.3.2.3,
/// §17.3.2.8). Checked against Word: it cannot open a document with a
/// `w:bdo` or a `w:dir` that has no `w:val`, nor one whose `w:bdo` names a
/// direction other than `ltr` or `rtl`; with `ltr` or `rtl` it opens
/// without repair.
pub fn check_bidi_direction(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            let is_bdo = is_w_tag(el, "bdo");
            if !is_bdo && !is_w_tag(el, "dir") {
                return;
            }
            let direction = el
                .attributes
                .iter()
                .find(|(name, _)| name.local_name == "val")
                .map(|(_, value)| value.as_str());
            let problem = match direction {
                None => Some("has no w:val".to_string()),
                Some("ltr" | "rtl") => None,
                Some(other) if is_bdo => Some(format!("names the direction {other:?}")),
                Some(_) => None,
            };
            if let Some(problem) = problem {
                findings.push(ValidationFinding {
                    rule_id: "I-RUN-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-RUN-001: {part_path}: a w:{} {problem}; the direction is ltr or \
                         rtl. Word cannot open such a file",
                        local_name(&el.name)
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-DOC-006: a section does not end inside a table cell
// ---------------------------------------------------------------------------

/// A paragraph's `w:sectPr` ends a section there (ECMA-376 §17.6.18).
/// Checked against Word: it cannot open a document whose `w:sectPr` sits in
/// a paragraph inside a table cell; in a body paragraph it opens cleanly.
pub fn check_section_break_in_table_cell(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if !is_w_tag(el, "sectPr") || path.last().map(String::as_str) != Some("pPr") {
                return;
            }
            // The nearest cell or text box decides: a text box starts its
            // own content and is not judged here.
            let container = path
                .iter()
                .rev()
                .find(|ancestor| matches!(ancestor.as_str(), "tc" | "txbxContent"));
            if container.map(String::as_str) == Some("tc") {
                findings.push(ValidationFinding {
                    rule_id: "I-DOC-006",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-DOC-006: {part_path}: a w:sectPr sits in a paragraph inside a \
                         table cell. Word cannot open such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-RUN-002: no w:contentPart in a run
// ---------------------------------------------------------------------------

/// `w:contentPart` (ISO/IEC 29500 run content, `CT_Rel`) is not read by
/// Word. Checked against Word with the same paragraph with and without one:
/// Word repairs the file whatever the relationship names — an ink part as
/// Word itself writes it (under the customXml relationship), an image, an
/// OLE part (a compound file or not) — and when it names no relationship.
/// Word writes ink as `w14:contentPart` inside a drawing, which is another
/// element and is not judged here.
pub fn check_content_part_in_run(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "contentPart") {
                findings.push(ValidationFinding {
                    rule_id: "I-RUN-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-RUN-002: {part_path}: a run holds a w:contentPart; Word repairs \
                         such a file, whatever part it names"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-OBJ-001: an embedded object is not written as w:objectEmbed
// ---------------------------------------------------------------------------

/// `w:object` may name its embedded part with `w:objectEmbed` (ISO/IEC 29500
/// `CT_ObjectEmbed`); Word writes `o:OLEObject` inside the VML instead.
/// Checked against Word with the same object with and without the
/// `w:objectEmbed`: with it Word cannot open the file, whether the part it
/// names is a valid compound file, a few bytes that are not one, or empty,
/// and whether or not the shape carries a picture. Without it, and with
/// `o:OLEObject` naming the same part (valid or not), the file opens cleanly.
pub fn check_object_embed(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, path| {
            if is_w_tag(el, "objectEmbed") && path.last().map(String::as_str) == Some("object") {
                findings.push(ValidationFinding {
                    rule_id: "I-OBJ-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-OBJ-001: {part_path}: a w:object names its embedded part with \
                         w:objectEmbed; Word cannot open such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-DRAW-001 / I-DRAW-002: a drawing carries the pieces Word requires
// ---------------------------------------------------------------------------

const WORDPROCESSING_DRAWING_NS: &str =
    "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const DRAWING_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PICTURE_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const WORDPROCESSING_SHAPE_NS: &str =
    "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";

/// What Word requires of one drawing element: attributes that must be
/// present, and children (in the listed namespace) that must be present.
struct DrawingRequirement {
    namespace: &'static str,
    local: &'static str,
    /// The element is judged only under this parent, when one is named.
    parent: Option<&'static str>,
    attributes: &'static [&'static str],
    children: &'static [(&'static str, &'static str)],
}

/// Checked against Word by removing one attribute or one child at a time
/// from four drawings it opens cleanly (an inline picture, an anchored
/// picture, an inline text-box shape, an anchored text-box shape). Every
/// entry here is a removal after which Word repairs the document. Removals
/// Word opens cleanly are not listed: the `dist*` attributes,
/// `wp:effectExtent`, `wp:cNvGraphicFramePr`, the blip's `r:embed`,
/// `a:stretch`, `a:xfrm` and its children, `a:avLst`, and a picture's
/// `a:prstGeom`.
const DRAWING_REQUIREMENTS: &[DrawingRequirement] = &[
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "inline",
        parent: None,
        attributes: &[],
        children: &[
            (WORDPROCESSING_DRAWING_NS, "extent"),
            (WORDPROCESSING_DRAWING_NS, "docPr"),
            (DRAWING_NS, "graphic"),
        ],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "anchor",
        parent: None,
        attributes: &[
            "simplePos",
            "relativeHeight",
            "behindDoc",
            "locked",
            "layoutInCell",
            "allowOverlap",
        ],
        children: &[
            (WORDPROCESSING_DRAWING_NS, "simplePos"),
            (WORDPROCESSING_DRAWING_NS, "positionH"),
            (WORDPROCESSING_DRAWING_NS, "positionV"),
            (WORDPROCESSING_DRAWING_NS, "extent"),
            (WORDPROCESSING_DRAWING_NS, "docPr"),
            (DRAWING_NS, "graphic"),
        ],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "simplePos",
        parent: Some("anchor"),
        attributes: &["x", "y"],
        children: &[],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "positionH",
        parent: Some("anchor"),
        attributes: &["relativeFrom"],
        children: &[],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "positionV",
        parent: Some("anchor"),
        attributes: &["relativeFrom"],
        children: &[],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "extent",
        parent: None,
        attributes: &["cx", "cy"],
        children: &[],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "effectExtent",
        parent: None,
        attributes: &["l", "t", "r", "b"],
        children: &[],
    },
    DrawingRequirement {
        namespace: WORDPROCESSING_DRAWING_NS,
        local: "docPr",
        parent: None,
        attributes: &["id", "name"],
        children: &[],
    },
    DrawingRequirement {
        namespace: DRAWING_NS,
        local: "graphic",
        parent: None,
        attributes: &[],
        children: &[(DRAWING_NS, "graphicData")],
    },
    DrawingRequirement {
        namespace: DRAWING_NS,
        local: "graphicData",
        parent: Some("graphic"),
        attributes: &["uri"],
        children: &[],
    },
    DrawingRequirement {
        namespace: PICTURE_NS,
        local: "pic",
        parent: None,
        attributes: &[],
        children: &[
            (PICTURE_NS, "nvPicPr"),
            (PICTURE_NS, "blipFill"),
            (PICTURE_NS, "spPr"),
        ],
    },
    DrawingRequirement {
        namespace: PICTURE_NS,
        local: "nvPicPr",
        parent: None,
        attributes: &[],
        children: &[(PICTURE_NS, "cNvPr"), (PICTURE_NS, "cNvPicPr")],
    },
    DrawingRequirement {
        namespace: PICTURE_NS,
        local: "cNvPr",
        parent: Some("nvPicPr"),
        attributes: &["id", "name"],
        children: &[],
    },
    DrawingRequirement {
        namespace: PICTURE_NS,
        local: "blipFill",
        parent: Some("pic"),
        attributes: &[],
        children: &[(DRAWING_NS, "blip")],
    },
    DrawingRequirement {
        namespace: DRAWING_NS,
        local: "off",
        parent: Some("xfrm"),
        attributes: &["x", "y"],
        children: &[],
    },
    DrawingRequirement {
        namespace: DRAWING_NS,
        local: "ext",
        parent: Some("xfrm"),
        attributes: &["cx", "cy"],
        children: &[],
    },
    DrawingRequirement {
        namespace: DRAWING_NS,
        local: "prstGeom",
        parent: None,
        attributes: &["prst"],
        children: &[],
    },
];

fn drawing_prefix(namespace: &str) -> &'static str {
    match namespace {
        WORDPROCESSING_DRAWING_NS => "wp",
        DRAWING_NS => "a",
        PICTURE_NS => "pic",
        WORDPROCESSING_SHAPE_NS => "wps",
        _ => "w",
    }
}

const MARKUP_COMPATIBILITY_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

fn element_children(el: &Element) -> impl Iterator<Item = &Element> {
    el.children.iter().filter_map(|node| match node {
        XMLNode::Element(child) => Some(child),
        _ => None,
    })
}

/// The element's children as Word reads them: a child wrapped in
/// `mc:AlternateContent` (in any `mc:Choice` or the `mc:Fallback`) counts as
/// a child. Word writes an anchor's `wp:positionH`/`wp:positionV` that way
/// when the position is a percentage.
fn children_through_alternate_content(el: &Element) -> Vec<&Element> {
    let mut children = Vec::new();
    for child in element_children(el) {
        if child.namespace.as_deref() == Some(MARKUP_COMPATIBILITY_NS) {
            if local_name(&child.name) == "AlternateContent" {
                for branch in element_children(child) {
                    children.extend(children_through_alternate_content(branch));
                }
            }
        } else {
            children.push(child);
        }
    }
    children
}

/// The nearest ancestor that is not markup-compatibility wrapping.
fn parent_through_alternate_content(path: &[String]) -> Option<&str> {
    path.iter()
        .rev()
        .map(String::as_str)
        .find(|ancestor| !matches!(*ancestor, "AlternateContent" | "Choice" | "Fallback"))
}

fn has_child(el: &Element, namespace: &str, local: &str) -> bool {
    children_through_alternate_content(el).iter().any(|child| {
        child.namespace.as_deref() == Some(namespace) && local_name(&child.name) == local
    })
}

fn has_attribute(el: &Element, local: &str) -> bool {
    el.attributes
        .iter()
        .any(|(name, _)| name.local_name == local)
}

/// A drawing carries the attributes and children Word requires.
///
/// I-DRAW-001 (Word repairs the document): a missing required attribute or
/// child from [`DRAWING_REQUIREMENTS`]; an anchored drawing with no wrapping
/// element; a `wp:positionH`/`wp:positionV` with no position inside; a
/// `wps:wsp` in shape graphic data without `wps:spPr` or `wps:bodyPr`, with
/// neither `wps:cNvSpPr` nor `wps:cNvCnPr`, or whose `wps:spPr` names no
/// geometry; a `wp:docPr` whose `id` is not an unsigned
/// 32-bit number. (Two drawings sharing one `wp:docPr` id open cleanly, and
/// so does a complete picture with no image behind it.)
///
/// I-DRAW-002 (Word cannot open the document): a `w:drawing` that does not
/// hold exactly one `wp:inline` or `wp:anchor`, and an `a:graphicData` with
/// nothing inside.
pub fn check_drawing_required_parts(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        let mut repaired = Vec::new();
        let mut unopenable = Vec::new();
        walk_elements(root, &mut |el, path| {
            let local = local_name(&el.name);
            let namespace = el.namespace.as_deref().unwrap_or_default();
            let name = format!("{}:{local}", drawing_prefix(namespace));

            if is_w_tag(el, "drawing") {
                let children = children_through_alternate_content(el);
                let one_frame = children.len() == 1
                    && children[0].namespace.as_deref() == Some(WORDPROCESSING_DRAWING_NS)
                    && matches!(local_name(&children[0].name), "inline" | "anchor");
                if !one_frame {
                    unopenable.push(
                        "a w:drawing does not hold exactly one wp:inline or wp:anchor".to_string(),
                    );
                }
                return;
            }

            if let Some(requirement) = DRAWING_REQUIREMENTS.iter().find(|requirement| {
                requirement.namespace == namespace
                    && requirement.local == local
                    && requirement
                        .parent
                        .is_none_or(|parent| parent_through_alternate_content(path) == Some(parent))
            }) {
                for attribute in requirement.attributes {
                    if !has_attribute(el, attribute) {
                        repaired.push(format!("a {name} element has no {attribute} attribute"));
                    }
                }
                for (child_namespace, child) in requirement.children {
                    if !has_child(el, child_namespace, child) {
                        repaired.push(format!(
                            "a {name} element has no {}:{child} child",
                            drawing_prefix(child_namespace)
                        ));
                    }
                }
            }

            match (namespace, local) {
                (WORDPROCESSING_DRAWING_NS, "anchor") => {
                    let wrapped = children_through_alternate_content(el).iter().any(|child| {
                        child.namespace.as_deref() == Some(WORDPROCESSING_DRAWING_NS)
                            && matches!(
                                local_name(&child.name),
                                "wrapNone"
                                    | "wrapSquare"
                                    | "wrapTight"
                                    | "wrapThrough"
                                    | "wrapTopAndBottom"
                            )
                    });
                    if !wrapped {
                        repaired.push("a wp:anchor element has no wrapping element".to_string());
                    }
                }
                (WORDPROCESSING_DRAWING_NS, "positionH" | "positionV")
                    if parent_through_alternate_content(path) == Some("anchor") =>
                {
                    if element_children(el).next().is_none() {
                        repaired.push(format!("a {name} element holds no position"));
                    }
                }
                (WORDPROCESSING_DRAWING_NS, "docPr") => {
                    if let Some((_, id)) = el
                        .attributes
                        .iter()
                        .find(|(name, _)| name.local_name == "id")
                        && id.trim().parse::<u32>().is_err()
                    {
                        repaired.push(format!(
                            "a wp:docPr id {id:?} is not an unsigned 32-bit number"
                        ));
                    }
                }
                (DRAWING_NS, "graphicData")
                    if parent_through_alternate_content(path) == Some("graphic") =>
                {
                    let children = children_through_alternate_content(el);
                    if children.is_empty() {
                        unopenable.push("an a:graphicData element is empty".to_string());
                    }
                    // A shape is judged where Word reads one: directly in
                    // graphic data that declares the shape namespace. Word
                    // opens a wps:wsp left under another declared kind
                    // cleanly, whatever it lacks.
                    let declares_shape = el.attributes.iter().any(|(name, value)| {
                        name.local_name == "uri" && value == WORDPROCESSING_SHAPE_NS
                    });
                    for shape in children.iter().filter(|child| {
                        declares_shape
                            && child.namespace.as_deref() == Some(WORDPROCESSING_SHAPE_NS)
                            && local_name(&child.name) == "wsp"
                    }) {
                        for required in ["spPr", "bodyPr"] {
                            if !has_child(shape, WORDPROCESSING_SHAPE_NS, required) {
                                repaired
                                    .push(format!("a wps:wsp element has no wps:{required} child"));
                            }
                        }
                        if !has_child(shape, WORDPROCESSING_SHAPE_NS, "cNvSpPr")
                            && !has_child(shape, WORDPROCESSING_SHAPE_NS, "cNvCnPr")
                        {
                            repaired.push(
                                "a wps:wsp element has neither a wps:cNvSpPr nor a wps:cNvCnPr \
                                 child"
                                    .to_string(),
                            );
                        }
                        let geometry = children_through_alternate_content(shape)
                            .into_iter()
                            .filter(|child| {
                                child.namespace.as_deref() == Some(WORDPROCESSING_SHAPE_NS)
                                    && local_name(&child.name) == "spPr"
                            })
                            .all(|properties| {
                                has_child(properties, DRAWING_NS, "prstGeom")
                                    || has_child(properties, DRAWING_NS, "custGeom")
                            });
                        if !geometry {
                            repaired.push("a wps:spPr element names no geometry".to_string());
                        }
                    }
                }
                _ => {}
            }
        });
        for problem in repaired {
            findings.push(ValidationFinding {
                rule_id: "I-DRAW-001",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-DRAW-001: {part_path}: {problem}. Word reports the file as corrupted \
                     and repairs it"
                ),
                location: part_path.to_string(),
            });
        }
        for problem in unopenable {
            findings.push(ValidationFinding {
                rule_id: "I-DRAW-002",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-DRAW-002: {part_path}: {problem}. Word cannot open such a file"
                ),
                location: part_path.to_string(),
            });
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// I-SDT-002: a list item's display text is not empty
// ---------------------------------------------------------------------------

/// A `w:listItem` of a combo-box or drop-down content control does not
/// carry an empty `w:displayText` (I-SDT-002). Checked against Word: it
/// cannot open a document where one does, in a combo box or a drop-down
/// list, inline or around a paragraph, whatever the item's `w:value`. An
/// item with no `w:displayText` at all, with an empty `w:value`, or sharing
/// its value or display text with another item opens cleanly.
pub fn check_content_control_list_items(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if !is_w_tag(el, "listItem") {
                return;
            }
            let empty_display = el
                .attributes
                .iter()
                .any(|(name, value)| name.local_name == "displayText" && value.is_empty());
            if empty_display {
                findings.push(ValidationFinding {
                    rule_id: "I-SDT-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-SDT-002: {part_path}: a w:listItem has an empty w:displayText. Word \
                         cannot open such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-TBL-001 / I-TBL-002: a cell ends with a paragraph; a row holds a cell
// ---------------------------------------------------------------------------

/// A table cell holds block content and its last block is not a table
/// (ECMA-376 §17.4.66: a cell ends with a paragraph). Checked against Word:
/// it repairs a document with a cell that is empty, holds only `w:tcPr`,
/// holds only a table, or ends with a table; a cell ending with a paragraph
/// (after a nested table, or inside a content control) opens cleanly, and
/// so does one with a bookmark end after its last paragraph (I-TBL-001).
/// Word cannot open a document with a row that holds no cell (I-TBL-002).
pub fn check_table_cell_and_row_content(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "tc") {
                let last_block = element_children(el)
                    .filter(|child| {
                        ["p", "tbl", "sdt", "customXml"]
                            .iter()
                            .any(|block| is_w_tag(child, block))
                    })
                    .last();
                let problem = match last_block {
                    None => Some("holds no paragraph"),
                    Some(block) if is_w_tag(block, "tbl") => Some("ends with a table"),
                    Some(_) => None,
                };
                if let Some(problem) = problem {
                    findings.push(ValidationFinding {
                        rule_id: "I-TBL-001",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-TBL-001: {part_path}: a table cell {problem}; a cell ends with \
                             a paragraph. Word reports the file as corrupted and repairs it"
                        ),
                        location: part_path.to_string(),
                    });
                }
            } else if is_w_tag(el, "tr") && !contains_w_tag(el, "tc") {
                findings.push(ValidationFinding {
                    rule_id: "I-TBL-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-TBL-002: {part_path}: a table row holds no cell. Word cannot open \
                         such a file"
                    ),
                    location: part_path.to_string(),
                });
            }
        });
    }
    findings
}

fn contains_w_tag(el: &Element, local: &str) -> bool {
    element_children(el).any(|child| is_w_tag(child, local) || contains_w_tag(child, local))
}

// ---------------------------------------------------------------------------
// I-SDT-001: a single-paragraph content control holds one paragraph
// ---------------------------------------------------------------------------

/// A block-level content control of a kind that holds one value (plain
/// text, date, drop-down list, combo box, picture) holds exactly one
/// paragraph. Checked against Word: it repairs a document where such a
/// control wraps two paragraphs, or a table; the same control around one
/// paragraph opens cleanly, and a rich-text control (no kind element)
/// around two paragraphs opens cleanly. A plain-text control inside a
/// paragraph, around several runs, opens cleanly and is not judged.
pub fn check_content_control_single_paragraph(
    stories: &[(String, &Element)],
) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if !is_w_tag(el, "sdt") {
                return;
            }
            let child = |local: &str| element_children(el).find(|child| is_w_tag(child, local));
            let (Some(properties), Some(content)) = (child("sdtPr"), child("sdtContent")) else {
                return;
            };
            let Some(kind) = element_children(properties).find_map(|property| {
                ["text", "date", "dropDownList", "comboBox", "picture"]
                    .into_iter()
                    .find(|kind| is_w_tag(property, kind))
            }) else {
                return;
            };
            let paragraphs = element_children(content)
                .filter(|block| is_w_tag(block, "p"))
                .count();
            let tables = element_children(content)
                .filter(|block| is_w_tag(block, "tbl"))
                .count();
            // Block level only: a control around runs, cells or rows holds
            // no paragraph or table of its own. A control showing its
            // placeholder is clean around several (see sdt_shows_placeholder).
            if paragraphs + tables == 0
                || (paragraphs == 1 && tables == 0)
                || sdt_shows_placeholder(properties)
            {
                return;
            }
            findings.push(ValidationFinding {
                rule_id: "I-SDT-001",
                severity: ValidationSeverity::Error,
                message: format!(
                    "I-SDT-001: {part_path}: a w:{kind} content control holds {paragraphs} \
                     paragraph(s) and {tables} table(s); such a control holds exactly one \
                     paragraph. Word reports the file as corrupted and repairs it"
                ),
                location: part_path.to_string(),
            });
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// I-MC-001: every prefix named by mc:Ignorable is declared
// ---------------------------------------------------------------------------

/// `mc:Ignorable` lists namespace prefixes, and each must be declared on
/// the element (ISO/IEC 29500-3 §10.1). Checked against Word: it repairs a
/// document whose root names an undeclared prefix in `mc:Ignorable`, and
/// opens it cleanly once the prefix is declared or the list is empty.
pub fn check_ignorable_prefixes_declared(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        let Some((_, ignorable)) = root.attributes.iter().find(|(name, _)| {
            name.local_name == "Ignorable"
                && name.namespace.as_deref()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        }) else {
            continue;
        };
        for prefix in ignorable.split_whitespace() {
            let declared = root
                .namespaces
                .as_ref()
                .is_some_and(|namespaces| namespaces.get(prefix).is_some());
            if !declared {
                findings.push(ValidationFinding {
                    rule_id: "I-MC-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "I-MC-001: {part_path}: mc:Ignorable names the prefix {prefix:?}, \
                         which the root element does not declare. Word reports the file as \
                         corrupted and repairs it"
                    ),
                    location: part_path.to_string(),
                });
            }
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// I-RANGE-001: colFirst/colLast are a both-or-neither pair
// ---------------------------------------------------------------------------

/// On a `bookmarkStart` or `permStart`, `w:colFirst` and `w:colLast` define a
/// column-scoped range and must appear together; a lone one is non-conformant.
/// (ECMA-376 / ISO 29500-1 §17.13.6.2, §17.13.7.2.)
pub fn check_colfirst_collast_pairing(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_path, root) in stories {
        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "bookmarkStart") || is_w_tag(el, "permStart") {
                let has_first = attr_get(el, "w:colFirst").is_some();
                let has_last = attr_get(el, "w:colLast").is_some();
                if has_first != has_last {
                    findings.push(ValidationFinding {
                        rule_id: "I-RANGE-001",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "{} has {} without {}; w:colFirst and w:colLast are a \
                             both-or-neither pair (ISO 29500-1 §17.13.6.2/§17.13.7.2)",
                            local_name(&el.name),
                            if has_first { "w:colFirst" } else { "w:colLast" },
                            if has_first { "w:colLast" } else { "w:colFirst" },
                        ),
                        location: part_path.to_string(),
                    });
                }
            }
        });
    }
    findings
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Parse a WML XML snippet into an Element.
    fn parse_wml(xml: &str) -> Element {
        Element::parse(Cursor::new(xml.as_bytes())).expect("test XML should parse")
    }

    // -----------------------------------------------------------------------
    // I-ANN-001 / I-ANN-002
    // -----------------------------------------------------------------------

    #[test]
    fn ann_001_bookmark_pair_not_flagged() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkStart w:id="0" w:name="a"/>
                    <w:bookmarkEnd w:id="0"/>
                    <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>text</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_annotation_id_uniqueness(&stories);
        // bookmarkStart and bookmarkEnd share the same w:id by design (that's how
        // they pair). This is NOT a duplicate — it's a valid annotation pair.
        let dup: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-001")
            .collect();
        assert!(
            dup.is_empty(),
            "bookmark start/end pair should not be flagged as duplicate"
        );
    }

    #[test]
    fn ann_001_duplicate_across_stories() {
        let xml1 = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="5" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>a</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let xml2 = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:p>
                <w:del w:id="5" w:author="y" w:date="2024-01-01T00:00:00Z">
                    <w:r><w:delText>b</w:delText></w:r>
                </w:del>
            </w:p>
        </w:hdr>"#;
        let root1 = parse_wml(xml1);
        let root2 = parse_wml(xml2);
        let stories = vec![
            ("word/document.xml".to_string(), &root1),
            ("word/header1.xml".to_string(), &root2),
        ];
        let findings = check_annotation_id_uniqueness(&stories);
        let dups: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-001")
            .collect();
        assert_eq!(dups.len(), 1);
        assert!(dups[0].message.contains("5"));
        assert!(dups[0].message.contains("2 times"));
    }

    #[test]
    fn ann_002_invalid_id_value() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="abc" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>t</w:t></w:r>
                    </w:ins>
                    <w:del w:id="-1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>d</w:delText></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_annotation_id_uniqueness(&stories);
        let invalid: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-002")
            .collect();
        assert_eq!(invalid.len(), 2);
        assert!(invalid.iter().any(|f| f.message.contains("abc")));
        assert!(invalid.iter().any(|f| f.message.contains("-1")));
    }

    #[test]
    fn ann_002_valid_ids_no_findings() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="0" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>t</w:t></w:r>
                    </w:ins>
                    <w:del w:id="4294967295" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>d</w:delText></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_annotation_id_uniqueness(&stories);
        let invalid: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-002")
            .collect();
        assert!(invalid.is_empty(), "0 and u32::MAX are valid");
    }

    // -----------------------------------------------------------------------
    // I-ANN-003: Bookmark pairing
    // -----------------------------------------------------------------------

    #[test]
    fn ann_003_matched_pairs_no_findings() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkStart w:id="0" w:name="a"/>
                    <w:r><w:t>text</w:t></w:r>
                    <w:bookmarkEnd w:id="0"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_bookmark_pairing(&stories);
        assert!(
            findings.is_empty(),
            "matched pair should produce no findings"
        );
    }

    #[test]
    fn ann_003_missing_end() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkStart w:id="7" w:name="orphan"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_bookmark_pairing(&stories);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ANN-003");
        assert!(findings[0].message.contains("no matching bookmarkEnd"));
    }

    #[test]
    fn ann_003_missing_start() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkEnd w:id="9"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_bookmark_pairing(&stories);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ANN-003");
        assert!(findings[0].message.contains("no matching bookmarkStart"));
    }

    #[test]
    fn ann_003_cross_story_not_paired() {
        let xml1 = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkStart w:id="3" w:name="cross"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let xml2 = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:p>
                <w:bookmarkEnd w:id="3"/>
            </w:p>
        </w:hdr>"#;
        let root1 = parse_wml(xml1);
        let root2 = parse_wml(xml2);
        let stories = vec![
            ("word/document.xml".to_string(), &root1),
            ("word/header1.xml".to_string(), &root2),
        ];
        let findings = check_bookmark_pairing(&stories);
        // Each story should report an unmatched bookmark
        assert_eq!(findings.len(), 2, "start and end are in different stories");
    }

    // -----------------------------------------------------------------------
    // I-DOC-001 / I-DOC-002 / I-DOC-003
    // -----------------------------------------------------------------------

    #[test]
    fn doc_001_valid_root() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p/>
                <w:sectPr/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let findings = check_document_root(&root);
        assert!(
            findings.is_empty(),
            "valid document should produce no findings"
        );
    }

    #[test]
    fn doc_001_wrong_root() {
        let xml =
            r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;
        let root = parse_wml(xml);
        let findings = check_document_root(&root);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-DOC-001");
    }

    #[test]
    fn doc_002_no_body() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        </w:document>"#;
        let root = parse_wml(xml);
        let findings = check_document_root(&root);
        let doc002: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-DOC-002")
            .collect();
        assert_eq!(doc002.len(), 1);
    }

    #[test]
    fn doc_002_multiple_bodies() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body><w:sectPr/></w:body>
            <w:body><w:sectPr/></w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let findings = check_document_root(&root);
        let doc002: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-DOC-002")
            .collect();
        assert_eq!(doc002.len(), 1);
        assert!(doc002[0].message.contains("2"));
    }

    #[test]
    fn doc_003_missing_sect_pr() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let findings = check_document_root(&root);
        let doc003: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-DOC-003")
            .collect();
        assert_eq!(doc003.len(), 1);
        assert!(doc003[0].message.contains("sectPr"));
    }

    #[test]
    fn doc_003_sect_pr_not_last() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:sectPr/>
                <w:p/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let findings = check_document_root(&root);
        let doc003: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-DOC-003")
            .collect();
        assert_eq!(doc003.len(), 1);
    }

    // -----------------------------------------------------------------------
    // I-TC-001 / I-TC-002
    // -----------------------------------------------------------------------

    #[test]
    fn tc_001_valid_tracked_change_no_findings() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>added</w:t></w:r>
                    </w:ins>
                    <w:del w:id="2" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>removed</w:delText></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        assert!(
            findings.is_empty(),
            "valid content should produce no findings"
        );
    }

    #[test]
    fn tc_001_hyperlink_inside_ins() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:hyperlink>
                            <w:r><w:t>link</w:t></w:r>
                        </w:hyperlink>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert_eq!(tc001.len(), 1);
        assert!(tc001[0].message.contains("hyperlink"));
    }

    #[test]
    fn tc_001_fld_simple_inside_del() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="2" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:fldSimple w:instr="PAGE">
                            <w:r><w:t>1</w:t></w:r>
                        </w:fldSimple>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert_eq!(tc001.len(), 1);
        assert!(tc001[0].message.contains("fldSimple"));
    }

    #[test]
    fn tc_001_block_level_p_as_direct_child() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:p><w:r><w:t>nested para</w:t></w:r></w:p>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert_eq!(tc001.len(), 1);
        assert!(tc001[0].message.contains("not allowed"));
    }

    #[test]
    fn tc_001_tbl_as_direct_child() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="2" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:tbl/>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert_eq!(tc001.len(), 1);
        assert!(tc001[0].message.contains("tbl"));
    }

    #[test]
    fn tc_001_drawing_with_textbox_inside_ins_is_valid() {
        // Direct child is w:r (valid). Content nested inside the run —
        // drawings, text boxes, fields — is governed by CT_R, not
        // CT_RunTrackChange. No recursion into descendants.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r>
                            <w:drawing>
                                <w:txbxContent>
                                    <w:p>
                                        <w:fldSimple w:instr="DOCPROPERTY &quot;name&quot;">
                                            <w:r><w:t>value</w:t></w:r>
                                        </w:fldSimple>
                                    </w:p>
                                </w:txbxContent>
                            </w:drawing>
                        </w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert!(tc001.is_empty(), "w:r is a valid direct child: {tc001:?}");
    }

    #[test]
    fn tc_001_non_wml_direct_child_is_valid() {
        // mc:AlternateContent, m:oMath, etc. are non-WML elements allowed
        // by schema extensions — they should not be flagged.
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <mc:AlternateContent>
                            <mc:Choice Requires="w14">
                                <w:r><w:t>choice</w:t></w:r>
                            </mc:Choice>
                        </mc:AlternateContent>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert!(
            tc001.is_empty(),
            "mc:AlternateContent is non-WML: {tc001:?}"
        );
    }

    #[test]
    fn tc_001_body_level_ins_wrapping_paragraph_is_valid() {
        // Body-level w:ins/w:del uses CT_TrackChange, which allows block
        // content (w:p, w:tbl) as children. This is distinct from
        // paragraph-level CT_RunTrackChange.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                    <w:p><w:r><w:t>inserted paragraph</w:t></w:r></w:p>
                </w:ins>
                <w:del w:id="2" w:author="x" w:date="2024-01-01T00:00:00Z">
                    <w:p><w:r><w:delText>deleted paragraph</w:delText></w:r></w:p>
                    <w:tbl/>
                </w:del>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert!(
            tc001.is_empty(),
            "body-level tracked changes allow block content: {tc001:?}"
        );
    }

    #[test]
    fn tc_001_math_context_rpr_inside_ins_is_valid() {
        // Tracked changes inside math runs (m:r) use a different content
        // model — w:rPr + m:t are valid inside w:ins per OOXML math extension.
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <w:body>
                <w:p>
                    <m:oMath>
                        <m:r>
                            <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                                <w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/></w:rPr>
                                <m:t>2</m:t>
                            </w:ins>
                        </m:r>
                    </m:oMath>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert!(
            tc001.is_empty(),
            "math context tracked changes allow w:rPr: {tc001:?}"
        );
    }

    #[test]
    fn tc_001_tc_level_ins_wrapping_paragraph_is_valid() {
        // Table cell-level w:ins/w:del also uses CT_TrackChange.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:tbl>
                    <w:tr>
                        <w:tc>
                            <w:ins w:id="1" w:author="x" w:date="2024-01-01T00:00:00Z">
                                <w:p><w:r><w:t>inserted</w:t></w:r></w:p>
                            </w:ins>
                        </w:tc>
                    </w:tr>
                </w:tbl>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_tracked_change_content_model(&stories);
        let tc001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-001")
            .collect();
        assert!(
            tc001.is_empty(),
            "tc-level tracked changes allow block content: {tc001:?}"
        );
    }

    #[test]
    fn tc_002_missing_id() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>t</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_required_tracked_change_ids(&stories);
        let tc002: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-002")
            .collect();
        assert_eq!(tc002.len(), 1);
        assert!(tc002[0].message.contains("missing"));
    }

    // -----------------------------------------------------------------------
    // I-ANN-004: paraId range validation
    // -----------------------------------------------------------------------

    #[test]
    fn check_para_id_valid_no_findings() {
        // 0x3B2A1C4D < 0x80000000 — valid
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <w:body>
                <w:p w14:paraId="3B2A1C4D"/>
                <w:sectPr/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_para_id_range(&stories);
        assert!(
            findings.is_empty(),
            "valid paraId should produce no findings: {findings:?}"
        );
    }

    #[test]
    fn check_para_id_no_attr_no_findings() {
        // Elements without w14:paraId should not be flagged
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p/>
                <w:sectPr/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_para_id_range(&stories);
        assert!(
            findings.is_empty(),
            "no paraId means no findings: {findings:?}"
        );
    }

    #[test]
    fn check_para_id_boundary_value_flagged() {
        // 0x80000000 is exactly the boundary — must be flagged
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <w:body>
                <w:p w14:paraId="80000000"/>
                <w:sectPr/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_para_id_range(&stories);
        let ann004: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-004")
            .collect();
        assert_eq!(ann004.len(), 1);
        assert!(ann004[0].message.contains("80000000"));
    }

    #[test]
    fn check_para_id_max_value_flagged() {
        // 0xFFFFFFFF is well above the limit — must be flagged
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <w:body>
                <w:p w14:paraId="FFFFFFFF"/>
                <w:sectPr/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_para_id_range(&stories);
        let ann004: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-004")
            .collect();
        assert_eq!(ann004.len(), 1);
        assert!(ann004[0].message.contains("FFFFFFFF"));
    }

    #[test]
    fn check_para_id_unparseable_flagged() {
        // Non-hex value must be flagged as an error
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <w:body>
                <w:p w14:paraId="GGGGGGGG"/>
                <w:sectPr/>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_para_id_range(&stories);
        let ann004: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-004")
            .collect();
        assert_eq!(ann004.len(), 1);
        assert!(ann004[0].message.contains("GGGGGGGG"));
    }

    #[test]
    fn tc_002_empty_id() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="" w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>d</w:delText></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_required_tracked_change_ids(&stories);
        let tc002: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-002")
            .collect();
        assert_eq!(tc002.len(), 1);
        assert!(tc002[0].message.contains("empty"));
    }

    #[test]
    fn tc_002_property_changes_require_ids() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:pPr>
                        <w:pPrChange w:author="x" w:date="2024-01-01T00:00:00Z">
                            <w:pPr/>
                        </w:pPrChange>
                    </w:pPr>
                </w:p>
                <w:sectPr>
                    <w:sectPrChange w:author="x" w:date="2024-01-01T00:00:00Z">
                        <w:sectPr/>
                    </w:sectPrChange>
                </w:sectPr>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_required_tracked_change_ids(&stories);
        let tc002: Vec<_> = findings
            .iter()
            .filter(|finding| finding.rule_id == "I-TC-002")
            .collect();

        assert_eq!(tc002.len(), 2, "both property changes require w:id");
        assert!(
            tc002
                .iter()
                .any(|finding| finding.message.contains("pPrChange"))
        );
        assert!(
            tc002
                .iter()
                .any(|finding| finding.message.contains("sectPrChange"))
        );
    }

    // -----------------------------------------------------------------------
    // I-ANN-005: Comment marker pairing
    // -----------------------------------------------------------------------

    #[test]
    fn ann_005_matched_pairs_no_findings() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:commentRangeStart w:id="0"/>
                    <w:r><w:t>text</w:t></w:r>
                    <w:commentRangeEnd w:id="0"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_comment_marker_pairing(&stories);
        assert!(
            findings.is_empty(),
            "matched pair should produce no findings"
        );
    }

    #[test]
    fn ann_005_orphaned_start() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:commentRangeStart w:id="3"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_comment_marker_pairing(&stories);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ANN-005");
        assert_eq!(findings[0].severity, ValidationSeverity::Error);
        assert!(findings[0].message.contains("no matching commentRangeEnd"));
    }

    #[test]
    fn ann_005_orphaned_end() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:commentRangeEnd w:id="7"/>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_comment_marker_pairing(&stories);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ANN-005");
        assert_eq!(findings[0].severity, ValidationSeverity::Error);
        assert!(
            findings[0]
                .message
                .contains("no matching commentRangeStart")
        );
    }

    #[test]
    fn ann_010_duplicate_balanced_ranges_and_references_are_errors() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:commentRangeStart w:id="9"/>
                    <w:commentRangeEnd w:id="9"/>
                    <w:r><w:commentReference w:id="9"/></w:r>
                    <w:commentRangeStart w:id="9"/>
                    <w:commentRangeEnd w:id="9"/>
                    <w:r><w:commentReference w:id="9"/></w:r>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_wml(xml);
        let stories = vec![("word/document.xml".to_string(), &root)];

        let findings = check_comment_marker_pairing(&stories);

        assert_eq!(findings.len(), 3, "one finding per duplicated marker role");
        assert!(findings.iter().all(|finding| finding.rule_id == "I-ANN-010"
            && finding.severity == ValidationSeverity::Error));
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("commentReference"))
        );
    }

    // -----------------------------------------------------------------------
    // I-ANN-006: Footnote/endnote ID range (MS-OI29500 §2.1.300-302)
    //
    // Word only allows footnote/endnote IDs in [-2147483648, 32767].
    // IDs above 32767 are rejected by Word even though ECMA-376 allows u32.
    // -----------------------------------------------------------------------

    #[test]
    fn ann_006_footnote_id_in_range_no_findings() {
        // w:id="100" is ≤ 32767 → valid, no findings expected
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:r><w:footnoteReference w:id="100"/></w:r>
              </w:p></w:body>
            </w:document>"#,
        );
        let footnotes = parse_wml(
            r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:footnote w:id="100">
                <w:p><w:r><w:t>note text</w:t></w:r></w:p>
              </w:footnote>
            </w:footnotes>"#,
        );
        let stories = vec![
            ("word/document.xml".to_string(), &doc),
            ("word/footnotes.xml".to_string(), &footnotes),
        ];
        let findings = check_footnote_endnote_id_range(&stories);
        let ann006: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-006")
            .collect();
        assert!(ann006.is_empty(), "ID 100 is within Word's allowed range");
    }

    #[test]
    fn ann_006_footnote_id_exceeds_word_limit() {
        // w:id="40000" is > 32767 → must be flagged as I-ANN-006
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:r><w:footnoteReference w:id="40000"/></w:r>
              </w:p></w:body>
            </w:document>"#,
        );
        let footnotes = parse_wml(
            r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:footnote w:id="40000">
                <w:p><w:r><w:t>note text</w:t></w:r></w:p>
              </w:footnote>
            </w:footnotes>"#,
        );
        let stories = vec![
            ("word/document.xml".to_string(), &doc),
            ("word/footnotes.xml".to_string(), &footnotes),
        ];
        let findings = check_footnote_endnote_id_range(&stories);
        let ann006: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-006")
            .collect();
        // Both the body footnoteReference AND the footnotes.xml definition carry
        // the out-of-range id, so both are flagged — the check covers the
        // reference, not just the definition.
        assert_eq!(ann006.len(), 2, "ID 40000 exceeds Word's max of 32767");
        assert!(ann006.iter().all(|f| f.message.contains("40000")));
        assert!(
            ann006
                .iter()
                .any(|f| f.message.contains("footnoteReference"))
        );
        assert!(ann006.iter().any(|f| f.message.contains("<w:footnote>")));
    }

    #[test]
    fn ann_006_endnote_id_exceeds_word_limit() {
        // w:id="40000" on a w:endnote is > 32767 → must be flagged as I-ANN-006
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:r><w:endnoteReference w:id="40000"/></w:r>
              </w:p></w:body>
            </w:document>"#,
        );
        let endnotes = parse_wml(
            r#"<w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:endnote w:id="40000">
                <w:p><w:r><w:t>endnote text</w:t></w:r></w:p>
              </w:endnote>
            </w:endnotes>"#,
        );
        let stories = vec![
            ("word/document.xml".to_string(), &doc),
            ("word/endnotes.xml".to_string(), &endnotes),
        ];
        let findings = check_footnote_endnote_id_range(&stories);
        let ann006: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-006")
            .collect();
        // Both the body endnoteReference and the endnotes.xml definition are flagged.
        assert_eq!(
            ann006.len(),
            2,
            "ID 40000 on endnote exceeds Word's max of 32767"
        );
        assert!(ann006.iter().all(|f| f.message.contains("40000")));
        assert!(
            ann006
                .iter()
                .any(|f| f.message.contains("endnoteReference"))
        );
        assert!(ann006.iter().any(|f| f.message.contains("<w:endnote>")));
    }

    // -----------------------------------------------------------------------
    // I-TC-003: No nested tracked changes (MS-OI29500 §2.1.330, §2.1.334)
    //
    // Word does not support w:ins inside w:ins or w:del inside w:del.
    // -----------------------------------------------------------------------

    #[test]
    fn tc_003_no_nested_ins_no_findings() {
        // Single w:ins (not nested) → no findings
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
                  <w:r><w:t>inserted</w:t></w:r>
                </w:ins>
              </w:p></w:body>
            </w:document>"#,
        );
        let stories = vec![("word/document.xml".to_string(), &doc)];
        let findings = check_no_nested_tracked_changes(&stories);
        let tc003: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-003")
            .collect();
        assert!(
            tc003.is_empty(),
            "non-nested ins should produce no I-TC-003 findings"
        );
    }

    #[test]
    fn tc_003_nested_ins_inside_ins_rejected() {
        // w:ins directly inside another w:ins → Word rejects this
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
                  <w:ins w:id="2" w:author="B" w:date="2024-01-02T00:00:00Z">
                    <w:r><w:t>nested</w:t></w:r>
                  </w:ins>
                </w:ins>
              </w:p></w:body>
            </w:document>"#,
        );
        let stories = vec![("word/document.xml".to_string(), &doc)];
        let findings = check_no_nested_tracked_changes(&stories);
        let tc003: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-003")
            .collect();
        assert_eq!(tc003.len(), 1, "inner w:ins inside w:ins must be flagged");
    }

    #[test]
    fn tc_003_nested_del_inside_del_rejected() {
        // w:del directly inside another w:del → Word rejects this
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
                  <w:del w:id="2" w:author="B" w:date="2024-01-02T00:00:00Z">
                    <w:r><w:delText>nested</w:delText></w:r>
                  </w:del>
                </w:del>
              </w:p></w:body>
            </w:document>"#,
        );
        let stories = vec![("word/document.xml".to_string(), &doc)];
        let findings = check_no_nested_tracked_changes(&stories);
        let tc003: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-003")
            .collect();
        assert_eq!(tc003.len(), 1, "inner w:del inside w:del must be flagged");
    }

    #[test]
    fn tc_003_ins_inside_textbox_story_under_ins_is_not_nesting() {
        // A textbox's w:txbxContent is a separate story: a tracked change
        // inside it does not nest with the tracked change wrapping the
        // textbox's anchor run. Word resolves the two stories independently.
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
                  <w:r><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml"><v:textbox>
                    <w:txbxContent><w:p>
                      <w:ins w:id="2" w:author="B" w:date="2024-01-02T00:00:00Z">
                        <w:r><w:t>textbox story insertion</w:t></w:r>
                      </w:ins>
                    </w:p></w:txbxContent>
                  </v:textbox></v:shape></w:pict></w:r>
                </w:ins>
              </w:p></w:body>
            </w:document>"#,
        );
        let stories = vec![("word/document.xml".to_string(), &doc)];
        let findings = check_no_nested_tracked_changes(&stories);
        let tc003: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-003")
            .collect();
        assert!(
            tc003.is_empty(),
            "txbxContent starts a new story; no nesting. Got: {tc003:?}"
        );
    }

    // -----------------------------------------------------------------------
    // I-ANN-007: Comment range count limit (MS-OI29500 §2.1.315)
    //
    // Word allows at most 32767 comment ranges per document.
    // -----------------------------------------------------------------------

    #[test]
    fn ann_007_few_comment_ranges_no_findings() {
        // 3 commentRangeStart elements → well under 32767, no findings
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:commentRangeStart w:id="1"/>
                <w:commentRangeEnd w:id="1"/>
                <w:commentRangeStart w:id="2"/>
                <w:commentRangeEnd w:id="2"/>
                <w:commentRangeStart w:id="3"/>
                <w:commentRangeEnd w:id="3"/>
                <w:r><w:t>text</w:t></w:r>
              </w:p></w:body>
            </w:document>"#,
        );
        let stories = vec![("word/document.xml".to_string(), &doc)];
        let findings = check_comment_range_count(&stories);
        let ann007: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-007")
            .collect();
        assert!(
            ann007.is_empty(),
            "3 comment ranges is well under the 32767 limit"
        );
    }

    #[test]
    fn ann_007_comment_range_count_reported() {
        // Verify the function counts correctly: 3 commentRangeStart elements → count = 3.
        // The function should return a finding only when count > 32767, so this expects no error.
        // This test confirms the counting logic is correct for a small known input.
        let doc = parse_wml(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p>
                <w:commentRangeStart w:id="10"/>
                <w:commentRangeEnd w:id="10"/>
                <w:commentRangeStart w:id="11"/>
                <w:commentRangeEnd w:id="11"/>
                <w:commentRangeStart w:id="12"/>
                <w:commentRangeEnd w:id="12"/>
                <w:r><w:t>text</w:t></w:r>
              </w:p></w:body>
            </w:document>"#,
        );
        let stories = vec![("word/document.xml".to_string(), &doc)];
        let findings = check_comment_range_count(&stories);
        // No error for 3 ranges. If the function reports a diagnostic count finding,
        // it should not be a severity::Error — confirm no I-ANN-007 errors emitted.
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-ANN-007" && f.severity == ValidationSeverity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "3 ranges should not trigger an I-ANN-007 error"
        );
    }

    // -----------------------------------------------------------------------
    // I-ANN-008: Bookmark name length
    // -----------------------------------------------------------------------

    #[test]
    fn ann_008_bookmark_name_within_limit_no_findings() {
        let name_40 = "A".repeat(40);
        let xml = format!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:bookmarkStart w:id="0" w:name="{name_40}"/>
                        <w:bookmarkEnd w:id="0"/>
                    </w:p>
                </w:body>
            </w:document>"#
        );
        let root = parse_wml(&xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_bookmark_name_length(&stories);
        assert!(
            findings.is_empty(),
            "40-char name should produce no findings: {findings:?}"
        );
    }

    #[test]
    fn ann_008_bookmark_name_over_limit_produces_warning() {
        let name_41 = "B".repeat(41);
        let xml = format!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:bookmarkStart w:id="0" w:name="{name_41}"/>
                        <w:bookmarkEnd w:id="0"/>
                    </w:p>
                </w:body>
            </w:document>"#
        );
        let root = parse_wml(&xml);
        let stories = vec![("word/document.xml".to_string(), &root)];
        let findings = check_bookmark_name_length(&stories);
        assert_eq!(
            findings.len(),
            1,
            "41-char name should produce 1 finding: {findings:?}"
        );
        assert_eq!(findings[0].rule_id, "I-ANN-008");
        assert_eq!(findings[0].severity, ValidationSeverity::Warning);
        assert!(findings[0].message.contains("41"));
    }
}
