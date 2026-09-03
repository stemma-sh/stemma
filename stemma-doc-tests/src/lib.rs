//! Repository-only compile guards for canonical narrative documentation.
//!
//! Keeping these carriers in a non-published package lets the documentation
//! remain in its canonical repository location without making a published
//! crate depend on files outside its source archive.

#[cfg(doctest)]
#[doc = include_str!("../../docs/reference/embedding.md")]
pub struct EmbeddingPageSnippets;

#[cfg(doctest)]
#[doc = include_str!("../../docs/guide/persistence.md")]
pub struct PersistencePageSnippets;

#[cfg(doctest)]
#[doc = include_str!("../../docs/guide/concepts.md")]
pub struct ConceptsPageSnippets;

#[cfg(doctest)]
#[doc = include_str!("../../docs/guide/revisions.md")]
pub struct RevisionsPageSnippets;

#[cfg(doctest)]
#[doc = include_str!("../../stemma-engine/docs/user/guide.md")]
pub struct EngineUserGuideSnippets;
