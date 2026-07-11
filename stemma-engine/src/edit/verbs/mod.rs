//! Per-verb authoring logic. Each submodule owns one `EditStep`'s validate +
//! apply, keeping the central grammar/dispatch in `edit/mod.rs` thin so verbs
//! can be added in parallel without colliding. See `edit/AGENTS.md`.

pub(crate) mod block_content_controls;
pub(crate) mod blocks_to_table;
pub(crate) mod bookmarks;
pub(crate) mod cell_formatting;
pub(crate) mod comments;
pub(crate) mod content_controls;
pub(crate) mod equations;
pub(crate) mod fields_crossrefs;
pub(crate) mod find_replace;
pub(crate) mod footnotes;
pub(crate) mod form_fields;
pub(crate) mod headers_footers;
pub(crate) mod image_insert;
pub(crate) mod image_layout;
pub(crate) mod images;
pub(crate) mod metadata;
pub(crate) mod numbering;
pub(crate) mod opaque_text_edit;
pub(crate) mod page_setup;
pub(crate) mod paragraph_formatting;
pub(crate) mod row_formatting;
pub(crate) mod run_formatting;
pub(crate) mod sdt_text_fill;
pub(crate) mod style_defs;
pub(crate) mod styles;
pub(crate) mod table_formatting;
pub(crate) mod table_ops;
pub(crate) mod tables_merged;
pub(crate) mod textbox;
pub(crate) mod update_fields;
