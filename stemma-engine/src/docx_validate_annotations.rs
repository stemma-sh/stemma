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
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
    "cellIns",
    "cellDel",
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
        if is_w_element(child_el) && TRACKED_CHANGE_CONTENT_BOUNDARIES.contains(&child_local) {
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

        if is_w_element(child_el)
            // Text-form scope boundaries — w:t is legitimately allowed here.
            && (DELETED_TEXT_FORM_BOUNDARIES.contains(&child_local)
                // Paragraph-level-only elements are forbidden inside a tracked
                // change outright (reported by the forbidden-descendant check);
                // their inner text form is moot until they are removed, so we
                // don't descend and double-report it.
                || FORBIDDEN_DESCENDANTS_IN_TRACKED_CHANGE.contains(&child_local))
        {
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
/// the same `w:id` within the same story part, and vice versa.
pub fn check_comment_marker_pairing(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for (part_path, root) in stories {
        let mut starts: HashMap<String, u32> = HashMap::new();
        let mut ends: HashMap<String, u32> = HashMap::new();

        walk_elements(root, &mut |el, _path| {
            if is_w_tag(el, "commentRangeStart") {
                if let Some(id) = attr_get(el, "w:id") {
                    *starts.entry(id.clone()).or_insert(0) += 1;
                }
            } else if is_w_tag(el, "commentRangeEnd")
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
        for id in ends.keys() {
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
/// - **I-TC-002**: every `w:del`/`w:ins` must have a `w:id` attribute with a
///   non-empty value.
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

            // I-TC-002: del/ins must have w:id
            match attr_get(el, "w:id") {
                None => {
                    findings.push(ValidationFinding {
                        rule_id: "I-TC-002",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "I-TC-002: <{el_local}> is missing required w:id attribute"
                        ),
                        location: format!("{part_path} <{el_local}>"),
                    });
                }
                Some(val) if val.is_empty() => {
                    findings.push(ValidationFinding {
                        rule_id: "I-TC-002",
                        severity: ValidationSeverity::Error,
                        message: format!("I-TC-002: <{el_local}> has empty w:id attribute"),
                        location: format!("{part_path} <{el_local}>"),
                    });
                }
                Some(_) => {}
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

/// Word rejects two `m:oMath` misplacements (oracle-confirmed): an `m:oMath`
/// nested directly inside another `m:oMath` (Word repairs the file), and an
/// `m:oMath` placed as a direct child of `w:body`, outside any paragraph (Word
/// cannot open the file). Both are otherwise schema-detectable only by deep
/// content-model validation, so flag them explicitly.
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
    if is_omath {
        if in_omath {
            findings.push(ValidationFinding {
                rule_id: "I-MATH-001",
                severity: ValidationSeverity::Error,
                message: "m:oMath nested directly inside another m:oMath; Word repairs the file \
                     (ISO 29500-1 §22.1.2.77, MS-OI29500 §2.1.1687)"
                    .to_string(),
                location: part_path.to_string(),
            });
        }
        if parent_is_body {
            findings.push(ValidationFinding {
                rule_id: "I-MATH-002",
                severity: ValidationSeverity::Error,
                message: "m:oMath placed as a direct child of w:body, outside any paragraph; \
                     Word cannot open the file (ISO 29500-1 §22.1.2.77)"
                    .to_string(),
                location: part_path.to_string(),
            });
        }
    }
    let children_parent_is_body = local_name(&el.name) == "body";
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
        let findings = check_tracked_change_content_model(&stories);
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
        let findings = check_tracked_change_content_model(&stories);
        let tc002: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-TC-002")
            .collect();
        assert_eq!(tc002.len(), 1);
        assert!(tc002[0].message.contains("empty"));
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
