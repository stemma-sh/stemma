//! Faults Word repairs or refuses in the parts beside the stories: the
//! theme, the document properties, the settings, charts, and the XML
//! declaration of the main part. Each check was established against Word
//! with the same package with and without the one fault; each doc comment
//! says what was observed.

use xmltree::{Element, XMLNode};

use crate::docx::DocxArchive;
use crate::docx_package::RelationshipSet;

/// The internal targets of `source`'s relationships of `kind` (last segment
/// of the relationship type), resolved to part names.
fn related_parts(archive: &DocxArchive, source: &str, kind: &str) -> Vec<String> {
    let rels_path = crate::docx_package::rels_part_path(source);
    let Some(relationships) = archive
        .get(&rels_path)
        .and_then(|bytes| RelationshipSet::parse(bytes, &rels_path).ok())
    else {
        return Vec::new();
    };
    let base = crate::docx_package::part_dir(source);
    relationships
        .entries
        .iter()
        .filter(|relationship| {
            crate::docx_validate::relationship_is_kind(&relationship.rel_type, kind)
                && crate::docx_package::relationship_target_mode_is_internal(
                    relationship.target_mode.as_deref(),
                )
        })
        .map(|relationship| {
            let target = relationship.target.replace('\\', "/");
            match target.strip_prefix('/') {
                Some(absolute) => crate::docx_package::normalize_package_path(absolute),
                None => crate::docx_package::normalize_package_path(&format!("{base}{target}")),
            }
        })
        .collect()
}

/// The package-level parts of `kind` (from `_rels/.rels`).
fn package_parts(archive: &DocxArchive, kind: &str) -> Vec<String> {
    let Some(relationships) = archive
        .get(crate::docx_package::ROOT_RELS_PATH)
        .and_then(|bytes| RelationshipSet::parse(bytes, crate::docx_package::ROOT_RELS_PATH).ok())
    else {
        return Vec::new();
    };
    relationships
        .entries
        .iter()
        .filter(|relationship| {
            crate::docx_validate::relationship_is_kind(&relationship.rel_type, kind)
                && crate::docx_package::relationship_target_mode_is_internal(
                    relationship.target_mode.as_deref(),
                )
        })
        .map(|relationship| crate::docx_package::normalize_package_path(&relationship.target))
        .collect()
}

fn element_children(element: &Element) -> impl Iterator<Item = &Element> {
    element.children.iter().filter_map(|node| match node {
        XMLNode::Element(child) => Some(child),
        _ => None,
    })
}

/// All faults below, as `(part, message)`.
pub(crate) fn part_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    issues.extend(main_part_xml_version_issue(archive, main_part));
    issues.extend(theme_issues(archive, main_part));
    issues.extend(extended_properties_issues(archive));
    issues.extend(core_properties_namespace_issues(archive));
    issues.extend(settings_rsid_issues(archive, main_part));
    issues.extend(chart_issues(archive, main_part));
    issues.extend(external_part_issues(archive, main_part));
    issues.extend(styles_xml_version_issue(archive, main_part));
    issues.extend(settings_namespace_issues(archive, main_part));
    issues.extend(numbering_level_issues(archive, main_part));
    issues
}

/// Kinds of related part the main document reads as a package part.
/// Checked against Word: a styles, numbering, font table or settings
/// relationship marked `TargetMode="External"` makes Word repair the file.
const INTERNAL_ONLY: [&str; 4] = ["styles", "numbering", "fontTable", "settings"];

fn external_part_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    let rels_path = crate::docx_package::rels_part_path(main_part);
    let Some(relationships) = archive
        .get(&rels_path)
        .and_then(|bytes| RelationshipSet::parse(bytes, &rels_path).ok())
    else {
        return Vec::new();
    };
    relationships
        .entries
        .iter()
        .filter_map(|relationship| {
            let kind = INTERNAL_ONLY.iter().find(|kind| {
                crate::docx_validate::relationship_is_kind(&relationship.rel_type, kind)
            })?;
            (!crate::docx_package::relationship_target_mode_is_internal(
                relationship.target_mode.as_deref(),
            ))
            .then(|| {
                (
                    rels_path.clone(),
                    format!(
                        "the {kind} relationship {:?} is marked TargetMode=\"External\"; Word \
                         repairs such a file",
                        relationship.id
                    ),
                )
            })
        })
        .collect()
}

/// The XML declaration version of `bytes`, when it states one.
fn xml_version(bytes: &[u8]) -> Option<String> {
    let head = std::str::from_utf8(&bytes[..bytes.len().min(200)]).ok()?;
    let declaration = head.strip_prefix('\u{feff}').unwrap_or(head);
    let declaration = &declaration[..declaration.find("?>")?];
    if !declaration.starts_with("<?xml") {
        return None;
    }
    Some(
        declaration
            .split("version=")
            .nth(1)?
            .trim_start_matches(['"', '\''])
            .split(['"', '\''])
            .next()?
            .to_string(),
    )
}

/// The styles part's XML declaration names version 1.0. Checked against
/// Word: `version="1.1"` on the styles part makes Word repair the file
/// (on the main part, refuse it).
fn styles_xml_version_issue(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    related_parts(archive, main_part, "styles")
        .into_iter()
        .filter_map(|part| {
            let version = xml_version(archive.get(&part)?)?;
            (version != "1.0").then(|| {
                let message = format!(
                    "styles part {part} declares XML version {version:?}; Word repairs such a file"
                );
                (part, message)
            })
        })
        .collect()
}

/// An element of a Word namespace this reader does not know (a later
/// version's), in the settings and not made ignorable at the settings root.
/// Checked against Word: `w99:futureOption` (namespace
/// `.../office/word/2099/wordml`) appended to the settings without
/// `mc:Ignorable` makes Word repair the file.
fn settings_namespace_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for part in related_parts(archive, main_part, "settings") {
        let Some(root) = archive.get(&part).and_then(|b| Element::parse(b).ok()) else {
            continue;
        };
        let ignorable: Vec<&str> = root
            .attributes
            .iter()
            .find(|(name, _)| name.local_name == "Ignorable")
            .map(|(_, value)| value.split_whitespace().collect())
            .unwrap_or_default();
        for element in element_children(&root) {
            let Some(namespace) = element.namespace.as_deref() else {
                continue;
            };
            let known = crate::word_xml::KNOWN_OOXML_NAMESPACES
                .iter()
                .any(|(_, uri)| *uri == namespace)
                || crate::word_xml::CORE_NAMESPACE_URIS.contains(&namespace);
            let skipped = element
                .prefix
                .as_deref()
                .is_some_and(|prefix| ignorable.contains(&prefix));
            if namespace.starts_with("http://schemas.microsoft.com/office/word/")
                && !known
                && !skipped
            {
                issues.push((
                    part.clone(),
                    format!(
                        "settings element {:?} is in namespace {namespace:?}, which Word does not \
                         know, and mc:Ignorable does not name it; Word repairs such a file",
                        element.name
                    ),
                ));
            }
        }
    }
    issues
}

const ST_NUMBER_FORMAT: &[&str] = &[
    "decimal",
    "upperRoman",
    "lowerRoman",
    "upperLetter",
    "lowerLetter",
    "ordinal",
    "cardinalText",
    "ordinalText",
    "hex",
    "chicago",
    "ideographDigital",
    "japaneseCounting",
    "aiueo",
    "iroha",
    "decimalFullWidth",
    "decimalHalfWidth",
    "japaneseLegal",
    "japaneseDigitalTenThousand",
    "decimalEnclosedCircle",
    "decimalFullWidth2",
    "aiueoFullWidth",
    "irohaFullWidth",
    "decimalZero",
    "bullet",
    "ganada",
    "chosung",
    "decimalEnclosedFullstop",
    "decimalEnclosedParen",
    "decimalEnclosedCircleChinese",
    "ideographEnclosedCircle",
    "ideographTraditional",
    "ideographZodiac",
    "ideographZodiacTraditional",
    "taiwaneseCounting",
    "ideographLegalTraditional",
    "taiwaneseCountingThousand",
    "taiwaneseDigital",
    "chineseCounting",
    "chineseLegalSimplified",
    "chineseCountingThousand",
    "koreanDigital",
    "koreanCounting",
    "koreanLegal",
    "koreanDigital2",
    "vietnameseCounting",
    "russianLower",
    "russianUpper",
    "none",
    "numberInDash",
    "hebrew1",
    "hebrew2",
    "arabicAlpha",
    "arabicAbjad",
    "hindiVowels",
    "hindiConsonants",
    "hindiNumbers",
    "hindiCounting",
    "thaiLetters",
    "thaiNumbers",
    "thaiCounting",
    "bahtText",
    "dollarText",
    "custom",
];

/// A list level's number format and start value. Checked against Word: a
/// `w:numFmt` of `arabic` (outside ST_NumberFormat) or a `w:start` of `one`
/// makes Word repair the file; an unknown `w:lvlJc` or `w:suff` opens
/// cleanly and is not judged.
fn numbering_level_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for part in related_parts(archive, main_part, "numbering") {
        let Some(root) = archive.get(&part).and_then(|b| Element::parse(b).ok()) else {
            continue;
        };
        let levels = element_children(&root)
            .filter(|el| el.name == "abstractNum" || el.name == "num")
            .flat_map(|el| {
                element_children(el)
                    .filter(|child| child.name == "lvl" || child.name == "lvlOverride")
                    .flat_map(|child| {
                        std::iter::once(child)
                            .chain(element_children(child).filter(|grand| grand.name == "lvl"))
                    })
            });
        for level in levels {
            for property in element_children(level) {
                let value = property
                    .attributes
                    .iter()
                    .find(|(name, _)| name.local_name == "val")
                    .map(|(_, value)| value.as_str());
                let Some(value) = value else {
                    continue;
                };
                let wrong = match property.name.as_str() {
                    "numFmt" => !ST_NUMBER_FORMAT.contains(&value),
                    "start" => value.parse::<i64>().is_err(),
                    _ => false,
                };
                if wrong {
                    issues.push((
                        part.clone(),
                        format!(
                            "list level w:{}/@w:val {value:?} is not a value Word knows for it; \
                             Word repairs such a file",
                            property.name
                        ),
                    ));
                }
            }
        }
    }
    issues
}

/// The main part's XML declaration names version 1.0. Checked against
/// Word: `version="1.1"` on the main document part makes Word refuse the
/// file.
fn main_part_xml_version_issue(archive: &DocxArchive, main_part: &str) -> Option<(String, String)> {
    let version = xml_version(archive.get(main_part)?)?;
    (version != "1.0").then(|| {
        (
            main_part.to_string(),
            format!(
                "main document part {main_part} declares XML version {version:?}; Word \
                 cannot open such a file"
            ),
        )
    })
}

/// The colour scheme a theme requires. Checked against Word: a theme whose
/// `a:clrScheme` lacks `a:accent6` makes Word repair the file.
const THEME_COLORS: [&str; 12] = [
    "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6",
    "hlink", "folHlink",
];

fn theme_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for part in related_parts(archive, main_part, "theme") {
        let Some(root) = archive.get(&part).and_then(|b| Element::parse(b).ok()) else {
            continue;
        };
        let scheme = element_children(&root)
            .filter(|el| el.name == "themeElements")
            .flat_map(element_children)
            .find(|el| el.name == "clrScheme");
        let Some(scheme) = scheme else {
            continue;
        };
        for color in THEME_COLORS {
            if !element_children(scheme).any(|el| el.name == color) {
                issues.push((
                    part.clone(),
                    format!(
                        "theme {part:?} has no a:{color} in its colour scheme; Word repairs \
                         such a file"
                    ),
                ));
            }
        }
    }
    issues
}

/// Extended properties whose value Word was seen to need as a number:
/// `Pages` and `Words` holding a word make Word repair the file. Other
/// counts are not judged: a Word-clean document carries a `TotalTime` that
/// is not a whole number.
const EXTENDED_PROPERTY_COUNTS: [&str; 2] = ["Pages", "Words"];

/// The elements of the extended properties (CT_Properties).
const EXTENDED_PROPERTIES: [&str; 27] = [
    "Template",
    "Manager",
    "Company",
    "Pages",
    "Words",
    "Characters",
    "PresentationFormat",
    "Lines",
    "Paragraphs",
    "Slides",
    "Notes",
    "TotalTime",
    "HiddenSlides",
    "MMClips",
    "ScaleCrop",
    "HeadingPairs",
    "TitlesOfParts",
    "LinksUpToDate",
    "CharactersWithSpaces",
    "SharedDoc",
    "HyperlinkBase",
    "HLinks",
    "HyperlinksChanged",
    "DigSig",
    "Application",
    "AppVersion",
    "DocSecurity",
];

/// Faults in the extended properties part (`docProps/app.xml`). Checked
/// against Word: the part not well-formed, `Pages` or `Words` holding a
/// word, or an element the schema does not define, makes Word repair the
/// file; a negative `Pages` opens cleanly.
fn extended_properties_issues(archive: &DocxArchive) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for part in package_parts(archive, "extended-properties") {
        let Some(bytes) = archive.get(&part) else {
            continue;
        };
        let root = match Element::parse(bytes) {
            Ok(root) => root,
            Err(_) => {
                issues.push((
                    part.clone(),
                    format!(
                        "extended properties part {part:?} is not well-formed XML; Word repairs \
                         such a file"
                    ),
                ));
                continue;
            }
        };
        for property in element_children(&root) {
            if property.namespace.as_deref()
                == Some("http://schemas.openxmlformats.org/officeDocument/2006/extended-properties")
                && !EXTENDED_PROPERTIES.contains(&property.name.as_str())
            {
                issues.push((
                    part.clone(),
                    format!(
                        "extended properties hold an element {:?} the schema does not define; \
                         Word repairs such a file",
                        property.name
                    ),
                ));
                continue;
            }
            if !EXTENDED_PROPERTY_COUNTS.contains(&property.name.as_str()) {
                continue;
            }
            let value = property
                .get_text()
                .map(|text| text.trim().to_string())
                .unwrap_or_default();
            if value.parse::<i64>().is_err() {
                issues.push((
                    part.clone(),
                    format!(
                        "extended property {} holds {value:?}, which is not a number; Word \
                         repairs such a file",
                        property.name
                    ),
                ));
            }
        }
    }
    issues
}

const DUBLIN_CORE: &str = "http://purl.org/dc/elements/1.1/";

/// The core properties use the Dublin Core namespace as written in the
/// standard. Checked against Word: the namespace written without its
/// trailing `/` (so `dc:title` is in another namespace) makes Word repair
/// the file.
fn core_properties_namespace_issues(archive: &DocxArchive) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for part in package_parts(archive, "core-properties") {
        let Some(root) = archive.get(&part).and_then(|b| Element::parse(b).ok()) else {
            continue;
        };
        let misplaced = element_children(&root).find(|el| {
            el.prefix.as_deref() == Some("dc") && el.namespace.as_deref() != Some(DUBLIN_CORE)
        });
        if let Some(element) = misplaced {
            issues.push((
                part.clone(),
                format!(
                    "core property dc:{} is in namespace {:?}, not {DUBLIN_CORE:?}; Word \
                     repairs such a file",
                    element.name,
                    element.namespace.as_deref().unwrap_or("")
                ),
            ));
        }
    }
    issues
}

/// Revision save ids in the settings are hexadecimal. Checked against Word:
/// a `w:rsids/w:rsid` value of `ZZ12XY34` makes Word repair the file.
fn settings_rsid_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for part in related_parts(archive, main_part, "settings") {
        let Some(root) = archive.get(&part).and_then(|b| Element::parse(b).ok()) else {
            continue;
        };
        let values = element_children(&root)
            .filter(|el| el.name == "rsids")
            .flat_map(element_children)
            .filter_map(|rsid| {
                rsid.attributes
                    .iter()
                    .find(|(name, _)| name.local_name == "val")
                    .map(|(_, value)| value.clone())
            });
        for value in values {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                issues.push((
                    part.clone(),
                    format!(
                        "settings revision save id {value:?} is not hexadecimal; Word repairs \
                         such a file"
                    ),
                ));
            }
        }
    }
    issues
}

/// A chart part the main document relates to is well-formed XML. Checked
/// against Word: a chart part cut to two thirds of its bytes makes Word
/// refuse the file.
fn chart_issues(archive: &DocxArchive, main_part: &str) -> Vec<(String, String)> {
    related_parts(archive, main_part, "chart")
        .into_iter()
        .filter(|part| {
            archive
                .get(part)
                .is_some_and(|bytes| Element::parse(bytes).is_err())
        })
        .map(|part| {
            let message =
                format!("chart part {part:?} is not well-formed XML; Word cannot open such a file");
            (part, message)
        })
        .collect()
}
