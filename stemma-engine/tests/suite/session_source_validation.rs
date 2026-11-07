//! A session reports how its open-time package validated: the report describes
//! the bytes that were opened, whatever the session has done since.

use std::io::Write;

use stemma::edit::{
    BarrierPolicy, EditTransaction, ExpectedMatches, MatchMode, MaterializationMode,
    ReplaceTextOptions, ReplaceTextScope, plan_replace_text,
};
use stemma::{DocxRuntime, RevisionInfo, SimpleRuntime};

/// A one-paragraph package carrying a part with no declared content type, an
/// OPC package invariant the validator reports (§10.1.2).
fn docx_with_undeclared_part() -> Vec<u8> {
    let parts = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/document.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>The term is one year.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#,
        ),
        ("word/notes.dat", "opaque"),
    ];
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        for (name, data) in parts {
            zip.start_file(name, zip::write::FileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn session_source_validation_is_the_open_time_packages_and_survives_edits() {
    let bytes = docx_with_undeclared_part();
    let opened = stemma::api::validate(&bytes);
    assert!(
        !opened.ok,
        "the fixture carries a package finding: {opened:?}"
    );

    let runtime = SimpleRuntime::new();
    let handle = runtime.import_docx(&bytes).expect("import").doc_handle;
    assert_eq!(runtime.session_source_validation(&handle).unwrap(), opened);

    let canonical = runtime
        .with(&handle, |snapshot| snapshot.canonical.as_ref().clone())
        .unwrap();
    let plan = plan_replace_text(
        &canonical,
        &ReplaceTextOptions {
            old: "one year".to_string(),
            new: "two years".to_string(),
            author: "Editor".to_string(),
            scope: ReplaceTextScope::WholeDoc,
            expected: ExpectedMatches::Count(1),
            match_mode: MatchMode::Exact,
            on_barrier_match: BarrierPolicy::Skip,
        },
    )
    .expect("plan");
    runtime
        .apply_edit(
            &handle,
            &EditTransaction {
                steps: plan.steps,
                summary: None,
                materialization_mode: MaterializationMode::TrackedChange,
                revision: RevisionInfo {
                    revision_id: 0,
                    identity: 0,
                    author: Some("Editor".to_string()),
                    date: None,
                    apply_op_id: None,
                },
            },
        )
        .expect("apply");
    assert_eq!(
        runtime.session_source_validation(&handle).unwrap(),
        opened,
        "an edit changes the session, not how its source validated"
    );
}
