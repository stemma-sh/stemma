//! Stemma: a typed-IR DOCX compiler.
//!
//! Parses DOCX into a canonical IR, diffs and merges with tracked-change
//! semantics, applies typed edit transactions, and serializes back to DOCX.
//!
//! # Entity model
//!
//! Think of stemma as a compiler over a long-lived document:
//!
//! | Compiler concept | Stemma type | Durability |
//! |---|---|---|
//! | source file | DOCX bytes (`&[u8]`) | **durable** — the only authoritative artifact |
//! | parser | [`crate::import`] | pure function |
//! | AST | [`CanonDoc`] | ephemeral, engine-version-bound |
//! | linker resources | `PackageScaffold` (unmodeled OOXML parts) | ephemeral |
//! | compilation unit | [`EditSnapshot`] (IR + scaffold) | ephemeral |
//! | edit/refactor spec | [`crate::edit::EditTransaction`] | **durable** — small JSON |
//! | code generator | [`crate::serialize`] / [`SimpleRuntime::export_docx`] | pure function |
//! | build cache | [`SimpleRuntime`]'s handle store | in-memory only |
//! | diff output | [`DocumentDiff`], [`ApplyResult`] | derived |
//!
//! ## What's durable, what isn't
//!
//! **Persist the DOCX bytes + the edit transactions.** Together those are
//! sufficient to reconstruct any past state — replay the transactions from
//! a stored DOCX baseline whenever a cold session needs to come back.
//!
//! **Do not persist the IR or an `EditSnapshot`.** The IR is engine-version-
//! bound — any change to the IR structs in a future engine release would
//! turn stored snapshots into a migration problem. Keep snapshots hot in
//! the runtime's handle store, evict cold ones with
//! [`SimpleRuntime::evict_expired`], and re-derive on cold access via
//! [`SimpleRuntime::import_docx`].
//!
//! The `export_snapshot_blob` / `import_snapshot_blob` pair on
//! [`SimpleRuntime`] exists for in-process / short-TTL handoff between
//! workers in the same engine build. Treat them as a hot-cache handoff,
//! not as a storage format. See their doc comments for the explicit
//! warning.
//!
//! ## Session model
//!
//! [`SimpleRuntime`] is one (opinionated) session implementation: a
//! `DashMap<DocHandle, EditSnapshot>` with last-accessed timestamps and an
//! `evict_expired(ttl)` method. Local-MCP deployments can use it directly;
//! hosted multi-tenant deployments wrap it with their own eviction policy.
//! Either way, the engine itself owns no durable state.

// ===========================================================================
// Public surface — semver scope for v0.1.0
// ===========================================================================
//
// Stemma's *intended* public surface is the [`api::Document`] facade. Build a
// `Document` from DOCX bytes, call verbs, export bytes back. New code should
// depend on `api` and treat everything below it as subject to change.
//
// Modules live in four tiers:
//
// 1. **The facade** — [`mod@api`]. The stable, documented v0.1.0 surface.
//
// 2. **The typed IR / domain model** — [`mod@domain`], [`mod@diff`],
//    [`mod@table`], [`mod@table_diff`], [`mod@tracked_model`],
//    [`mod@vocabulary`], [`mod@semantic_hash`], [`mod@redline_extract`],
//    [`mod@roundtrip_compare`]. The typed CanonDoc and its derived/diff
//    views. Downstream redline/diff pipelines
//    build on these directly. They are public but engine-version-bound: do
//    not persist the IR (see the crate docs above).
//
// 3. **The engine API (UNSTABLE)** — [`mod@edit`], [`mod@edit_v4`],
//    [`mod@view`], [`mod@html`], [`mod@extended_markdown`], [`mod@import`],
//    [`mod@runtime`], plus the OOXML part-level modules that downstream
//    consumers still drive directly: [`mod@docx`], [`mod@docx_validate`],
//    [`mod@docx_validate_annotations`], [`mod@docprops`],
//    [`mod@manual_markup`], [`mod@normalize`], [`mod@numbering`]. These are a
//    DELIBERATE engine API that the in-workspace `stemma-mcp` server and
//    downstream redline pipelines drive (both predate and underlie the
//    facade). They are NOT the stable surface and may change between minor
//    versions. Reach for `api::Document` unless you are inside the workspace
//    and specifically need transaction/view/part plumbing. See `README.md`
//    ("Public surface") for the rationale.
//
// 4. **Sealed internals** (`pub(crate)`) — everything below: OOXML
//    (de)serializer plumbing, the validator's xref/namespace/ordering
//    sub-checks, the styles/settings/word_ir part builders, the OPC package
//    writer. No external consumer reaches these; they are referenced only via
//    `crate::` inside stemma. Sealing them holds the semver surface to the
//    tiers above instead of ~1700 accidental items.

// --- Tier 1: the facade ---------------------------------------------------
pub mod api;
pub mod audit;

// --- Tier 2: the typed IR / domain model ----------------------------------
pub mod diff;
pub mod domain;
pub mod redline_extract;
pub mod roundtrip_compare;
pub mod semantic_hash;
pub mod table;
pub mod table_diff;
pub mod tracked_model;
pub mod vocabulary;

// --- Tier 3: the engine API (UNSTABLE — `stemma-mcp` + downstream consumers
// drive these directly; they may change between minor versions) -----------------
pub mod docprops;
pub mod docx;
pub mod docx_validate;
pub mod docx_validate_annotations;
pub mod edit;
pub mod edit_v4;
pub mod extended_markdown;
pub mod html;
pub mod import;
pub mod manual_markup;
pub mod normalize;
pub mod numbering;
pub mod opaque_meta;
pub(crate) mod opaque_splice;
pub mod opaque_targets;
pub(crate) mod resolution_rules;
pub mod runtime;
pub mod view;

// --- Tier 4: sealed internals — no external consumer ----------------------
// OOXML part builders, validator sub-checks, and the OPC package writer.
// Referenced only via `crate::` inside stemma. Sealed to keep the semver
// surface to the tiers above.
pub(crate) mod compat;
pub(crate) mod docx_package;
pub(crate) mod docx_validate_namespaces;
pub(crate) mod docx_validate_ordering;
pub(crate) mod docx_validate_xref;
pub(crate) mod serialize;
pub(crate) mod settings;
pub(crate) mod styles;
pub(crate) mod word_ir;
// Internal OOXML plumbing — not part of the public surface. No consumer
// (stemma-mcp, downstream apps) reaches these; they are referenced only via
// `crate::` inside stemma. Sealed to keep the semver surface to the facade.
pub(crate) mod word_xml;
pub(crate) mod xml_attrs;
pub(crate) mod xml_write;

// The stateless certification verb (value namespace) — callable as
// `stemma::audit(before, after)` alongside the `audit` MODULE (type
// namespace), which holds the report types.
pub use api::audit;
pub use audit::*;
pub use diff::*;
pub use domain::*;
pub use import::{build_canonical_from_docx_preserving_tracked, build_image_data_lookup};
pub use runtime::*;
pub use semantic_hash::*;
// Opaque style-table handle for re-resolving style-inherited run marks on
// accept/reject outside the runtime projection (see `reject_all_with_styles`).
// The `styles` module stays crate-private; only this token is public.
pub use styles::StyleTable;
pub use table::*;
pub use table_diff::*;
pub use tracked_model::*;
