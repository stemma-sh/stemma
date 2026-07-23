//! `docs/reference/operations.md` is GENERATED from the engine's operation
//! catalog (`stemma::edit_v4::catalog`). This test is both the generator and
//! the drift guard:
//!
//! - `operations_reference_is_current` (runs in the gate) renders the page
//!   from the live catalog and fails if the checked-in file differs, naming
//!   the fix.
//! - `regenerate_operations_reference` (ignored) writes the rendered page;
//!   run it via `just regen-operations-reference`.
//!
//! The rendered Markdown must satisfy `scripts/check-docs.py` (no em/en
//! dashes, no spaced dash punctuation, resolvable links); the style assertions
//! here mirror those rules so a catalog edit that would redden the docs gate
//! fails loudly in this crate first.

use std::fmt::Write as _;
use std::path::PathBuf;

use stemma::edit_v4::catalog::{OperationSpec, content_node_catalog, operation_catalog};

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/reference/operations.md")
}

/// `"formatting_and_styles"` renders as `"Formatting and styles"`.
fn group_title(group: &str) -> String {
    let words = group.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => panic!("catalog group names are non-empty"),
    }
}

fn render() -> String {
    let catalog = operation_catalog();
    // Group in first-appearance order over the parser table, so the page
    // order is deterministic and mirrors the catalog itself.
    let mut groups: Vec<(&str, Vec<&OperationSpec>)> = Vec::new();
    for spec in &catalog {
        match groups.iter_mut().find(|(name, _)| *name == spec.group) {
            Some((_, members)) => members.push(spec),
            None => groups.push((spec.group, vec![spec])),
        }
    }

    let mut page = String::new();
    let _ = write!(
        page,
        "\
# v4 operation reference

<!-- GENERATED FILE. Do not edit by hand: this page is rendered from the
     engine's operation catalog (stemma-engine/src/edit_v4/catalog.rs) by
     stemma-engine/tests/operations_reference.rs, and that test fails the
     gate when the page drifts. Regenerate with:

         just regen-operations-reference
-->

Every operation a v4 edit transaction accepts, rendered from the engine's own
parser table so this page cannot disagree with what the parser enforces.
Deserialization is strict per the
[stability contract](../guide/stability.md#v4-transaction-json-additive-unknown-fields-rejected):
an unknown field or an unknown `op` tag is a hard error, never a silent no-op,
so author against exactly the fields listed here.

The same catalog is served at runtime by every transport: over MCP via
`inspect_docx` with `query:\"operations\"` (which also lists that transport's
edge-only image `path` fields), and over HTTP at `GET /api/operations` (see
the [HTTP API reference](http.md#endpoints)). Placeholders like `<block_id>`
in the shapes below are yours to fill; each shape is otherwise schema-valid
verbatim, pinned by an engine test.

## Transaction envelope

A transaction is atomic: every op applies or none do. Targets are block ids
from the current document; `expect`, `guard`, or `semantic_hash` provides
optimistic concurrency where an op supports it.

```json
{{\"ops\":[{{\"op\":\"...\"}}],\"revision\":{{\"author\":\"J. Osei\"}},\"summary\":\"optional\"}}
```

| Field | Meaning |
|---|---|
| `ops` | Non-empty ordered operation array; each op is tagged by its snake_case `op` field. |
| `revision.author` | Required author stamped on every tracked change the transaction produces. |
| `revision.date` | Optional ISO-8601 timestamp. |
| `revision.apply_op_id` | Optional group id stamped on every change. |
| `summary` | Optional human-readable description. |
| `materialization_mode` | `tracked_change` (the default) or `direct`. |

`allow_existing_author` is NOT a transaction field: continuing an author who
already owns revisions in the document is a per-call assertion made on the
transport (the `allow_existing_author` tool argument over MCP, the
`?allow_existing_author=true` query parameter on HTTP `/apply`), never part
of the durable edit format. See the
[AuthorImpersonation refusal](mcp-advanced.md#refusal-vocabulary).

## Content nodes

The `content` field of `replace`, `insert`, `edit_header`, and `edit_footer`
takes the nodes below, each shown here inside a complete op.

One `marks` vocabulary is used everywhere: an ARRAY of tagged objects
(`[{{\"type\":\"bold\"}}]`), never bare strings. A text node's `marks` authors
inline content; `set_format`'s `marks` replaces a span's mark set, with the
value-carrying formatting (color, font, size) in sibling fields.
"
    );

    for node in content_node_catalog() {
        let _ = write!(
            page,
            "\n### `{name}` node\n\n{cue}\n",
            name = node.name,
            cue = node.cue,
        );
        for shape in node.examples {
            let _ = write!(page, "\n```json\n{shape}\n```\n");
        }
    }

    let _ = write!(
        page,
        "\n## Catalog

{count} operations in {group_count} groups:

",
        count = catalog.len(),
        group_count = groups.len(),
    );

    for (group, members) in &groups {
        let names: Vec<String> = members.iter().map(|s| format!("`{}`", s.name)).collect();
        let _ = writeln!(
            page,
            "* [{title}](#{anchor}): {names}",
            title = group_title(group),
            anchor = group.replace('_', "-"),
            names = names.join(", "),
        );
    }

    for (group, members) in &groups {
        let _ = write!(page, "\n## {}\n", group_title(group));
        for spec in members {
            let fields: Vec<String> = spec.fields.iter().map(|f| format!("`{f}`")).collect();
            let _ = write!(
                page,
                "\n### `{name}`\n\n{cue}\n\nFields: {fields}.\n",
                name = spec.name,
                cue = spec.cue,
                fields = fields.join(", "),
            );
            // Canonical compact shapes verbatim: the same bytes the MCP
            // schema-error path and `GET /api/operations` teach, so every
            // surface shows one identical canonical form.
            for shape in spec.examples {
                let _ = write!(page, "\n```json\n{shape}\n```\n");
            }
        }
    }

    let _ = write!(
        page,
        "\n## Related\n\n\
* [Stability and compatibility](../guide/stability.md): how this schema evolves.\n\
* [Read model reference](read-model.md): the read half, what you render a document from.\n\
* [MCP advanced reference](mcp-advanced.md): the transaction inside the tool surface.\n\
* [HTTP API reference](http.md): `POST /api/documents/{{id}}/apply` takes one of these transactions.\n"
    );

    for (line_number, line) in page.lines().enumerate() {
        for banned in ["\u{2014}", "\u{2013}", " - "] {
            assert!(
                !line.contains(banned),
                "rendered page line {} contains sequence {banned:?} banned by scripts/check-docs.py: {line}",
                line_number + 1
            );
        }
        assert!(
            !line.ends_with(" -"),
            "rendered page line {} ends with a dash, banned by scripts/check-docs.py: {line}",
            line_number + 1
        );
    }
    page
}

#[test]
fn operations_reference_is_current() {
    let want = render();
    let have = std::fs::read_to_string(doc_path()).unwrap_or_else(|e| {
        panic!(
            "docs/reference/operations.md is missing ({e}); run `just regen-operations-reference`"
        )
    });
    assert!(
        have == want,
        "docs/reference/operations.md is stale relative to the engine op catalog; \
         run `just regen-operations-reference` and commit the result"
    );
}

#[test]
#[ignore = "writes docs/reference/operations.md; run via `just regen-operations-reference`"]
fn regenerate_operations_reference() {
    let path = doc_path();
    std::fs::write(&path, render()).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
