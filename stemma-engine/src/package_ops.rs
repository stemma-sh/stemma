//! Verified, comparison-independent package operations.
//!
//! These operations are useful whenever an explicit edit requires package
//! state to be created or imported. Comparison may request them, but does not
//! own their validation, transport identities, or exact output projection.

pub mod generated_people;
pub(crate) mod numbering_transport;
