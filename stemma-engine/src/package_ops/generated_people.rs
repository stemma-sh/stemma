//! Verified creation or append-only merge of the Word people sidecar.
//!
//! This is one explicit package operation in the active package plan. It is
//! deliberately narrow: existing person payloads remain opaque and exact;
//! only missing declared revision authors are appended.

use std::fmt;

use xmltree::Element;

use crate::docx_package::DocxPackage;
use crate::serialize::build_people_xml;

pub const PEOPLE_PART: &str = "word/people.xml";
pub const PEOPLE_RELATIONSHIP_TYPE: &str =
    "http://schemas.microsoft.com/office/2011/relationships/people";
pub const PEOPLE_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.people+xml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedGeneratedPeopleOperation {
    #[cfg(test)]
    authors: Vec<String>,
    people_xml: Vec<u8>,
    relationship_id: String,
    creates_binding: bool,
}

impl VerifiedGeneratedPeopleOperation {
    #[cfg(test)]
    pub(crate) fn authors(&self) -> &[String] {
        &self.authors
    }

    #[cfg(test)]
    pub(crate) fn people_xml(&self) -> &[u8] {
        &self.people_xml
    }

    #[cfg(test)]
    pub(crate) fn relationship_id(&self) -> &str {
        &self.relationship_id
    }

    /// Apply the already-validated operation to the package it was planned
    /// against. Relationship allocation is checked rather than guessed: a
    /// changed transport state is an error, not a cue to pick another id.
    pub fn apply_to(&self, output: &mut DocxPackage) -> Result<(), GeneratedPeopleOperationError> {
        let mut next = output.clone();
        next.set_part(PEOPLE_PART, self.people_xml.clone());
        if self.creates_binding {
            let actual_relationship_id = next
                .document_rels
                .add(PEOPLE_RELATIONSHIP_TYPE, "people.xml");
            if actual_relationship_id != self.relationship_id {
                return Err(GeneratedPeopleOperationError::RelationshipIdChanged {
                    planned: self.relationship_id.clone(),
                    actual: actual_relationship_id,
                });
            }
            next.content_types
                .add_override(&format!("/{PEOPLE_PART}"), PEOPLE_CONTENT_TYPE);
        }
        *output = next;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeneratedPeopleOperationError {
    EmptyAuthorSet,
    EmptyAuthor {
        index: usize,
    },
    AuthorsNotStrictlySorted {
        index: usize,
        previous: String,
        current: String,
    },
    GeneratedXmlInvalid {
        detail: String,
    },
    PeoplePartAlreadyPresent,
    PeopleRelationshipAlreadyPresent {
        relationship_id: String,
        target: String,
        target_mode: Option<String>,
    },
    PeoplePartTargetAlreadyClaimed {
        relationship_id: String,
        relationship_type: String,
        target: String,
    },
    PeopleContentTypeAlreadyDeclared {
        declared_content_type: String,
    },
    ExistingPeopleBindingInvalid {
        detail: String,
    },
    RelationshipIdChanged {
        planned: String,
        actual: String,
    },
}

impl fmt::Display for GeneratedPeopleOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "generated people package operation failed: {self:?}"
        )
    }
}

impl std::error::Error for GeneratedPeopleOperationError {}

/// Plan the exact people.xml package delta emitted for a non-empty, sorted,
/// unique tracked-author set.
///
/// The author ordering is a precondition rather than something this operation
/// repairs: callers must pass the canonical order derived from the redline.
pub fn plan_generated_people_operation(
    base: &DocxPackage,
    authors: &[String],
) -> Result<VerifiedGeneratedPeopleOperation, GeneratedPeopleOperationError> {
    validate_authors(authors)?;
    let existing_people_xml = base.get_part(PEOPLE_PART);
    let creates_binding = existing_people_xml.is_none();
    let (people_xml, relationship_id) = if let Some(existing) = existing_people_xml {
        let relationship_id = validate_existing_people_state(base)?;
        let merged = crate::serialize::merge_people_xml(existing, authors).map_err(|detail| {
            GeneratedPeopleOperationError::ExistingPeopleBindingInvalid { detail }
        })?;
        (merged, relationship_id)
    } else {
        validate_creation_preconditions(base)?;
        let people_xml = build_people_xml(authors).into_bytes();
        let mut expected = base.clone();
        expected.set_part(PEOPLE_PART, people_xml.clone());
        let relationship_id = expected
            .document_rels
            .add(PEOPLE_RELATIONSHIP_TYPE, "people.xml");
        (people_xml, relationship_id)
    };
    Element::parse(people_xml.as_slice()).map_err(|error| {
        GeneratedPeopleOperationError::GeneratedXmlInvalid {
            detail: error.to_string(),
        }
    })?;

    Ok(VerifiedGeneratedPeopleOperation {
        #[cfg(test)]
        authors: authors.to_vec(),
        people_xml,
        relationship_id,
        creates_binding,
    })
}

fn validate_existing_people_state(
    base: &DocxPackage,
) -> Result<String, GeneratedPeopleOperationError> {
    let related: Vec<_> = base
        .document_rels
        .entries
        .iter()
        .filter(|relationship| {
            relationship.rel_type == PEOPLE_RELATIONSHIP_TYPE
                || (relationship.target_mode.as_deref() != Some("External")
                    && base
                        .document_rels
                        .resolve_internal_target(&relationship.target)
                        .eq_ignore_ascii_case(PEOPLE_PART))
        })
        .collect();
    let [relationship] = related.as_slice() else {
        return Err(
            GeneratedPeopleOperationError::ExistingPeopleBindingInvalid {
                detail: format!(
                    "existing people part has {} candidate document relationships; expected one",
                    related.len()
                ),
            },
        );
    };
    if relationship.rel_type != PEOPLE_RELATIONSHIP_TYPE
        || !crate::docx_package::relationship_target_mode_is_internal(
            relationship.target_mode.as_deref(),
        )
        || !base
            .document_rels
            .resolve_internal_target(&relationship.target)
            .eq_ignore_ascii_case(PEOPLE_PART)
    {
        return Err(
            GeneratedPeopleOperationError::ExistingPeopleBindingInvalid {
                detail: format!(
                    "existing people relationship {:?} has the wrong type, target, or mode",
                    relationship.id
                ),
            },
        );
    }
    let overrides: Vec<_> = base
        .content_types
        .overrides
        .iter()
        .filter(|entry| {
            entry
                .part_name
                .trim_start_matches('/')
                .eq_ignore_ascii_case(PEOPLE_PART)
        })
        .collect();
    if overrides.len() != 1 || overrides[0].content_type != PEOPLE_CONTENT_TYPE {
        return Err(
            GeneratedPeopleOperationError::ExistingPeopleBindingInvalid {
                detail: format!(
                    "existing people part has {} exact overrides; expected one {PEOPLE_CONTENT_TYPE:?}",
                    overrides
                        .iter()
                        .filter(|entry| entry.content_type == PEOPLE_CONTENT_TYPE)
                        .count()
                ),
            },
        );
    }
    Ok(relationship.id.clone())
}

fn validate_authors(authors: &[String]) -> Result<(), GeneratedPeopleOperationError> {
    if authors.is_empty() {
        return Err(GeneratedPeopleOperationError::EmptyAuthorSet);
    }
    for (index, author) in authors.iter().enumerate() {
        if author.is_empty() {
            return Err(GeneratedPeopleOperationError::EmptyAuthor { index });
        }
    }
    for (index, pair) in authors.windows(2).enumerate() {
        if pair[0] >= pair[1] {
            return Err(GeneratedPeopleOperationError::AuthorsNotStrictlySorted {
                index: index + 1,
                previous: pair[0].clone(),
                current: pair[1].clone(),
            });
        }
    }
    Ok(())
}

fn validate_creation_preconditions(
    base: &DocxPackage,
) -> Result<(), GeneratedPeopleOperationError> {
    if base.has_part(PEOPLE_PART) {
        return Err(GeneratedPeopleOperationError::PeoplePartAlreadyPresent);
    }
    if let Some(relationship) = base
        .document_rels
        .entries
        .iter()
        .find(|relationship| relationship.rel_type == PEOPLE_RELATIONSHIP_TYPE)
    {
        return Err(
            GeneratedPeopleOperationError::PeopleRelationshipAlreadyPresent {
                relationship_id: relationship.id.clone(),
                target: relationship.target.clone(),
                target_mode: relationship.target_mode.clone(),
            },
        );
    }
    if let Some(relationship) = base.document_rels.entries.iter().find(|relationship| {
        relationship.target_mode.as_deref() != Some("External")
            && base
                .document_rels
                .resolve_internal_target(&relationship.target)
                .eq_ignore_ascii_case(PEOPLE_PART)
    }) {
        return Err(
            GeneratedPeopleOperationError::PeoplePartTargetAlreadyClaimed {
                relationship_id: relationship.id.clone(),
                relationship_type: relationship.rel_type.clone(),
                target: relationship.target.clone(),
            },
        );
    }
    if let Some(override_entry) = base.content_types.overrides.iter().find(|entry| {
        entry
            .part_name
            .trim_start_matches('/')
            .eq_ignore_ascii_case(PEOPLE_PART)
    }) {
        return Err(
            GeneratedPeopleOperationError::PeopleContentTypeAlreadyDeclared {
                declared_content_type: override_entry.content_type.clone(),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::{DocxArchive, DocxFile};
    use crate::docx_package::DocxPackage;

    const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

    fn package() -> DocxPackage {
        let archive = DocxArchive::from_parts(vec![
            file("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            file("_rels/.rels", ROOT_RELS.as_bytes()),
            file("word/_rels/document.xml.rels", DOCUMENT_RELS.as_bytes()),
            file("word/document.xml", b"<document/>"),
            file("word/styles.xml", b"<styles/>"),
        ]);
        DocxPackage::from_archive(&archive).expect("valid package")
    }

    fn file(name: &str, data: &[u8]) -> DocxFile {
        DocxFile {
            name: name.to_string(),
            data: data.to_vec(),
        }
    }

    fn authors() -> Vec<String> {
        vec!["Andreas & Team".to_string(), "Reviewer <Two>".to_string()]
    }

    #[test]
    fn applying_people_operation_preserves_unrelated_package_state() {
        let mut base = package();
        let operation = plan_generated_people_operation(&base, &authors()).expect("valid plan");
        base.set_part("word/styles.xml", b"<styles changed='true'/>".to_vec());
        operation
            .apply_to(&mut base)
            .expect("apply people operation");

        assert_eq!(
            base.get_part("word/styles.xml"),
            Some(b"<styles changed='true'/>".as_slice())
        );
        assert!(base.has_part(PEOPLE_PART));
        assert!(
            base.document_rels
                .entries
                .iter()
                .any(|relationship| relationship.rel_type == PEOPLE_RELATIONSHIP_TYPE)
        );
    }

    #[test]
    fn exact_generated_operation_applies_to_the_base() {
        let mut base = package();
        let authors = authors();
        let operation = plan_generated_people_operation(&base, &authors).expect("valid operation");
        operation.apply_to(&mut base).expect("apply operation");

        assert!(base.has_part(PEOPLE_PART));
        assert_eq!(operation.authors(), authors);
        assert_eq!(operation.relationship_id(), "rId5");
        assert!(
            std::str::from_utf8(operation.people_xml())
                .expect("generated XML is UTF-8")
                .contains("Andreas &amp; Team")
        );
    }

    #[test]
    fn changed_transport_state_is_refused() {
        let mut base = package();
        let authors = authors();
        let operation = plan_generated_people_operation(&base, &authors).expect("valid operation");
        base.set_part("word/footer1.xml", b"<footer/>".to_vec());
        assert_eq!(
            base.document_rels.add(
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
                "footer1.xml",
            ),
            operation.relationship_id()
        );
        let relationship_count = base.document_rels.entries.len();
        let override_count = base.content_types.overrides.len();
        assert!(matches!(
            operation.apply_to(&mut base),
            Err(GeneratedPeopleOperationError::RelationshipIdChanged { .. })
        ));
        assert!(!base.has_part(PEOPLE_PART));
        assert_eq!(base.document_rels.entries.len(), relationship_count);
        assert_eq!(base.content_types.overrides.len(), override_count);
    }

    #[test]
    fn authors_must_be_nonempty_sorted_and_unique() {
        let base = package();
        assert_eq!(
            plan_generated_people_operation(&base, &[]),
            Err(GeneratedPeopleOperationError::EmptyAuthorSet)
        );
        assert!(matches!(
            plan_generated_people_operation(&base, &["B".to_string(), "A".to_string()]),
            Err(GeneratedPeopleOperationError::AuthorsNotStrictlySorted { .. })
        ));
        assert!(matches!(
            plan_generated_people_operation(&base, &["A".to_string(), "A".to_string()]),
            Err(GeneratedPeopleOperationError::AuthorsNotStrictlySorted { .. })
        ));
    }

    #[test]
    fn invalid_xml_author_is_refused() {
        let base = package();
        assert!(matches!(
            plan_generated_people_operation(&base, &["invalid\u{0}author".to_string()]),
            Err(GeneratedPeopleOperationError::GeneratedXmlInvalid { .. })
        ));
    }

    #[test]
    fn existing_people_collection_is_extended_without_rebuilding_prior_people() {
        let mut base = package();
        base.set_part(
            PEOPLE_PART,
            build_people_xml(&["Existing".to_string()]).into_bytes(),
        );
        base.document_rels
            .add(PEOPLE_RELATIONSHIP_TYPE, "people.xml");
        base.content_types
            .add_override(&format!("/{PEOPLE_PART}"), PEOPLE_CONTENT_TYPE);

        let operation = plan_generated_people_operation(&base, &authors()).expect("merge plan");
        let merged = std::str::from_utf8(operation.people_xml()).expect("merged UTF-8 XML");
        assert!(merged.contains("Existing"));
        assert!(merged.contains("Andreas &amp; Team"));
        operation
            .apply_to(&mut base)
            .expect("apply merged collection");
        assert_eq!(base.get_part(PEOPLE_PART), Some(operation.people_xml()));
    }

    #[test]
    fn existing_people_part_without_its_binding_is_refused() {
        let mut base = package();
        base.set_part(
            PEOPLE_PART,
            build_people_xml(&["Existing".to_string()]).into_bytes(),
        );

        assert!(matches!(
            plan_generated_people_operation(&base, &authors()),
            Err(GeneratedPeopleOperationError::ExistingPeopleBindingInvalid { .. })
        ));
    }

    #[test]
    fn existing_people_relationship_is_refused() {
        let mut base = package();
        base.document_rels
            .add(PEOPLE_RELATIONSHIP_TYPE, "other-people.xml");

        assert!(matches!(
            plan_generated_people_operation(&base, &authors()),
            Err(GeneratedPeopleOperationError::PeopleRelationshipAlreadyPresent { .. })
        ));
    }

    #[test]
    fn claimed_people_target_is_refused() {
        let mut base = package();
        base.document_rels.add("urn:other", "people.xml");

        assert!(matches!(
            plan_generated_people_operation(&base, &authors()),
            Err(GeneratedPeopleOperationError::PeoplePartTargetAlreadyClaimed { .. })
        ));
    }

    #[test]
    fn existing_people_content_type_is_refused() {
        let mut base = package();
        base.content_types
            .add_override("/word/people.xml", PEOPLE_CONTENT_TYPE);

        assert_eq!(
            plan_generated_people_operation(&base, &authors()),
            Err(
                GeneratedPeopleOperationError::PeopleContentTypeAlreadyDeclared {
                    declared_content_type: PEOPLE_CONTENT_TYPE.to_string(),
                }
            )
        );
    }
}
