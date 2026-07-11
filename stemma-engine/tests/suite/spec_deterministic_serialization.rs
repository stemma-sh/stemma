//! H1 — DETERMINISTIC SERIALIZATION (acceptance).
//!
//! Contract: serializing the same [`Document`] must produce byte-identical
//! output every time, in the same process and across processes. "Across
//! processes" is the load-bearing clause: `HashMap`/`HashSet` use a
//! per-process `RandomState`, so any iteration over an unordered container
//! whose order reaches the wire yields a different byte stream each run. That
//! is exactly the amplifier that made a past accept-path divergence bug
//! *intermittent* — it only reproduced when the per-process hash order lined
//! up a certain way.
//!
//! We cannot fork a second process from a unit test, but we do not need to:
//! `RandomState` is reseeded for every freshly constructed container, so
//! rebuilding the containers per serialize call within one process samples the
//! same order-instability a second process would. Each `Document::parse`
//! followed by a serialize builds its packages, id tables, media registries,
//! and namespace sets from scratch — so a reparse→reserialize cycle exercises
//! the cross-process concern. Where a path builds fresh containers per call
//! (the compare/merge media-copy registry, the notes-part namespace repair),
//! running it repeatedly and asserting byte-equality is the in-process proxy
//! for cross-process determinism.
//!
//! The battery deliberately spans the rich surfaces that carry unordered
//! collections to the wire: tracked changes (incl. moves via diff), numbering,
//! styles, comments/notes sidecars, images/media, and tables with formatting.
//!
//! Determinism has a second, non-hash source: the ZIP writer's per-entry
//! timestamp. With the `time` feature on, the zip crate stamps the current
//! wall-clock time into every local file header, so the same document
//! serialized seconds apart differs in the DOS date/time fields. That is pinned
//! to a fixed epoch, and asserted directly below.

use std::io::Write as _;

use stemma::api::Document;
use stemma::{DocxRuntime, ExportOptions, SimpleRuntime, TransactionMeta};
use zip::write::FileOptions;

/// Read a shipped test fixture by path relative to `testdata/`.
fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/testdata/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

/// Serialize with the default (Blocking-validated) export options, failing
/// loud with the fixture label on any error.
fn ser(label: &str, doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions::default())
        .unwrap_or_else(|e| panic!("serialize {label}: {e:?}"))
}

/// Assert that serializing `doc` twice is byte-identical (same-process, same
/// Document — the trivial floor: the package assembly + zip writer must not
/// reorder parts or attributes run-to-run).
fn assert_double_serialize_stable(label: &str, doc: &Document) {
    let a = ser(label, doc);
    let b = ser(label, doc);
    assert_eq!(
        a, b,
        "{label}: two serialize() calls on the SAME Document must be byte-identical"
    );
}

/// Assert steady-state determinism under reimport: parse→serialize repeatedly,
/// rebuilding every container from bytes each cycle (the in-process proxy for a
/// fresh process). The FIRST cycle may canonicalize the input, so we compare
/// the second and third cycles, which are both already canonical.
fn assert_reimport_steady_state(label: &str, bytes: &[u8]) {
    let c1 = ser(
        label,
        &Document::parse(bytes).unwrap_or_else(|e| panic!("parse {label}: {e:?}")),
    );
    let c2 = ser(
        label,
        &Document::parse(&c1).unwrap_or_else(|e| panic!("reparse {label} c1: {e:?}")),
    );
    let c3 = ser(
        label,
        &Document::parse(&c2).unwrap_or_else(|e| panic!("reparse {label} c2: {e:?}")),
    );
    assert_eq!(
        c2, c3,
        "{label}: steady-state reserialization must be byte-identical across reparse cycles \
         (fresh containers each cycle — a per-process hash order leaking to the wire would \
         diverge here)"
    );
}

/// Fixtures parsed and serialized directly. Each spans at least one unordered
/// collection that historically could reach the wire.
const SINGLE_DOC_BATTERY: &[&str] = &[
    "simple-text/before.docx",
    "simple-text/after.docx",
    "table-changes/before.docx",
    "table-changes/after.docx",
    "footnotes/before.docx",
    "footnotes/after.docx",
    "images/before.docx",
    "images/after.docx",
    "image-math-combined/before.docx",
    "math-equations/before.docx",
    "twenty-paragraphs/before.docx",
    "long-table/before.docx",
    "barriers/field.docx",
    "barriers/hyperlink.docx",
    "barriers/sdt.docx",
];

/// `(base, target)` fixture pairs diffed to a redline. Diffing routes through
/// the compare/merge serialization path, which copies target media for
/// inserted drawings and re-serializes every story part — the paths that build
/// fresh media/namespace containers per call.
const DIFF_BATTERY: &[(&str, &str)] = &[
    ("simple-text/before.docx", "simple-text/after.docx"),
    ("table-changes/before.docx", "table-changes/after.docx"),
    ("footnotes/before.docx", "footnotes/after.docx"),
    ("images/before.docx", "images/after.docx"),
    (
        "twenty-paragraphs/before.docx",
        "twenty-paragraphs/after.docx",
    ),
    ("long-table/before.docx", "long-table/after.docx"),
    ("math-equations/before.docx", "math-equations/after.docx"),
];

#[test]
fn single_documents_serialize_deterministically() {
    for &rel in SINGLE_DOC_BATTERY {
        let bytes = fixture(rel);
        let doc = Document::parse(&bytes).unwrap_or_else(|e| panic!("parse {rel}: {e:?}"));
        assert_double_serialize_stable(rel, &doc);
        assert_reimport_steady_state(rel, &bytes);
    }
}

#[test]
fn diffed_redlines_serialize_deterministically() {
    for &(base_rel, target_rel) in DIFF_BATTERY {
        let label = format!("diff({base_rel} -> {target_rel})");
        let base =
            Document::parse(&fixture(base_rel)).unwrap_or_else(|e| panic!("parse base: {e:?}"));
        let target =
            Document::parse(&fixture(target_rel)).unwrap_or_else(|e| panic!("parse target: {e:?}"));

        // Two INDEPENDENT diff runs (each builds its own compare-path
        // containers) must serialize to the same bytes: this is where a
        // per-process hash order in the merge/media path would surface.
        let redline_a = base
            .diff(&target)
            .unwrap_or_else(|e| panic!("{label} run a: {e:?}"));
        let redline_b = base
            .diff(&target)
            .unwrap_or_else(|e| panic!("{label} run b: {e:?}"));
        let a = ser(&label, &redline_a);
        let b = ser(&label, &redline_b);
        assert_eq!(
            a, b,
            "{label}: two independent diff() runs must serialize byte-identically"
        );

        assert_double_serialize_stable(&label, &redline_a);
        assert_reimport_steady_state(&label, &a);

        // Accept/reject projections of the redline must serialize
        // deterministically too — the projection rebuilds the tracked model
        // before serializing.
        let accepted = redline_a
            .read_accepted()
            .unwrap_or_else(|e| panic!("{label} accept: {e:?}"));
        let rejected = redline_a
            .read_rejected()
            .unwrap_or_else(|e| panic!("{label} reject: {e:?}"));
        assert_double_serialize_stable(&format!("{label} [accepted]"), &accepted);
        assert_double_serialize_stable(&format!("{label} [rejected]"), &rejected);
        assert_reimport_steady_state(&format!("{label} [accepted]"), &ser(&label, &accepted));
        assert_reimport_steady_state(&format!("{label} [rejected]"), &ser(&label, &rejected));
    }
}

/// One inline-image run, embedding `rid` via a DrawingML `a:blip`.
fn image_run(rid: &str, id: &str, name: &str) -> String {
    format!(
        r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="762106" cy="790685"/><wp:effectExtent l="0" t="0" r="0" b="9525"/><wp:docPr id="{id}" name="{name}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="{id}" name="{name}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="762106" cy="790685"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#
    )
}

/// Build a DOCX from a body fragment plus a list of `(rId, media-part-name,
/// bytes)` image relationships/parts. All the DrawingML namespaces the inline
/// images use are declared on the document root.
fn make_image_docx(body_inner: &str, images: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut doc_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (rid, part, _) in images {
        let target = part.strip_prefix("word/").unwrap_or(part);
        doc_rels.push_str(&format!(
            r#"<Relationship Id="{rid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{target}"/>"#
        ));
    }
    doc_rels.push_str("</Relationships>");

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(root_rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        for (_, part, bytes) in images {
            zip.start_file(*part, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The strongest exerciser of the media-copy ordering fix: compare a text-only
/// base against a target carrying two distinct inline images, so both become
/// inserted drawings whose media the merge path must copy into the base package
/// and whose rIds it must reallocate. That reallocation walks the set of
/// referenced rIds and assigns fresh rIds / `image_target_N` part names
/// sequentially; if the walk were hash-ordered, the rId→media-name assignment
/// (and therefore the bytes) would vary run to run.
///
/// This uses the runtime compare/redline path (`serialize_canonical_docx`) —
/// the only path that copies target media; `Document::diff` + `serialize`
/// re-emits cached scaffold bytes and never reaches the media copy. Each call
/// builds its media registry from scratch (fresh `RandomState`), so identical
/// results across many calls is the in-process proxy for cross-process
/// stability. Two rIds give two possible hash orders per call, so a regression
/// to hash order is caught with probability ≈ 1 − 2⁻²⁰ per test run.
#[test]
fn inserted_multi_image_media_copy_is_order_stable() {
    // Two distinct PNG payloads (content need only be non-empty and unequal —
    // the engine copies the bytes and links a relationship; it does not decode
    // the image).
    let png_a: &[u8] = b"\x89PNG\r\n\x1a\nSTEMMA-IMAGE-ALPHA-payload-0000";
    let png_b: &[u8] = b"\x89PNG\r\n\x1a\nSTEMMA-IMAGE-BETA-payload-1111";

    // Base and target share one paragraph; the target inserts two inline image
    // runs INSIDE it, so the drawings land in an inserted segment (the case the
    // merge-path media copy covers) rather than a wholesale-inserted block.
    let base = make_image_docx(
        r#"<w:p><w:r><w:t xml:space="preserve">Heading </w:t></w:r><w:r><w:t>end.</w:t></w:r></w:p>"#,
        &[],
    );
    let target_body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Heading </w:t></w:r>{}{}<w:r><w:t>end.</w:t></w:r></w:p>"#,
        image_run("rId100", "100", "Alpha"),
        image_run("rId101", "101", "Beta"),
    );
    let target = make_image_docx(
        &target_body,
        &[
            ("rId100", "word/media/alpha.png", png_a),
            ("rId101", "word/media/beta.png", png_b),
        ],
    );

    let runtime = SimpleRuntime::new();
    let base_handle = runtime
        .import_docx(&base)
        .expect("import synthetic text base");
    let target_handle = runtime
        .import_docx(&target)
        .expect("import synthetic two-image target");
    let meta = || TransactionMeta {
        author: "Determinism".to_string(),
        reason: None,
        timestamp_utc: Some("2026-01-01T00:00:00Z".to_string()),
    };

    let baseline = runtime
        .compare_and_redline(&base_handle.doc_handle, &target_handle.doc_handle, meta())
        .expect("compare text -> 2 images")
        .redline_bytes;

    // Sanity: the redline actually copied both images (else this asserts nothing
    // about ordering).
    let media_count = {
        let archive = stemma::docx::DocxArchive::read(&baseline).expect("read redline");
        archive
            .list()
            .filter(|n| n.starts_with("word/media/"))
            .count()
    };
    assert!(
        media_count >= 2,
        "precondition: the redline must carry both inserted images (found {media_count} media parts)"
    );

    for run in 0..20 {
        let bytes = runtime
            .compare_and_redline(&base_handle.doc_handle, &target_handle.doc_handle, meta())
            .expect("compare text -> 2 images")
            .redline_bytes;
        assert_eq!(
            baseline, bytes,
            "multi-image compare run {run}: copying media for inserted drawings must assign \
             rIds and media-part names in a stable (document) order, not hash order"
        );
    }
}

/// Byte-level determinism also requires a fixed per-entry ZIP timestamp: with
/// the `time` feature enabled, the zip crate's default `FileOptions` stamps the
/// current wall-clock time into every local file header, so two serializations
/// of the same document seconds apart differ in the DOS date/time fields. This
/// is the subtle, non-hash source of nondeterminism — the double-serialize
/// checks above only catch it when the two calls straddle the 2-second DOS-time
/// boundary. Assert directly that every emitted entry carries the fixed
/// 1980-01-01 epoch, so a regression to wall-clock stamping fails deterministically.
#[test]
fn serialized_zip_entries_use_fixed_epoch_timestamp() {
    let bytes = ser(
        "simple-text/before.docx",
        &Document::parse(&fixture("simple-text/before.docx")).unwrap(),
    );
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("read zip");
    assert!(!zip.is_empty(), "expected a non-empty package");
    for i in 0..zip.len() {
        let entry = zip.by_index(i).expect("zip entry");
        let t = entry.last_modified();
        assert_eq!(
            (
                t.year(),
                t.month(),
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            ),
            (1980, 1, 1, 0, 0, 0),
            "zip entry {:?} must carry the fixed 1980 epoch, not a wall-clock timestamp",
            entry.name()
        );
    }
}
