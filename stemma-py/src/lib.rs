//! Python bindings for the Stemma engine (`pip install stemma-py`, `import
//! stemma`).
//!
//! A thin transport over the Tier 1 [`stemma::api::Document`] facade and the
//! `stemma-diff` comparison facade, in the same role as the CLI and the MCP
//! server: transactions arrive as v4 JSON and go through the engine's own
//! parser (`edit_v4::parse_transaction` → `into_edit_transaction`); results
//! leave as plain Python values (dicts, lists, str, bytes). Every method maps
//! to exactly one facade call; nothing here retries, defaults, or reinterprets
//! an engine answer (CLAUDE.md: no silent fallbacks).
//!
//! Output dicts are built by exhaustive destructuring of the engine types, so
//! a field added to any of them is a compile error here rather than a value
//! silently missing from Python.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use pyo3::create_exception;
use pyo3::exceptions::{
    PyException, PyFileExistsError, PyFileNotFoundError, PyIsADirectoryError, PyOSError,
    PyPermissionError, PyRuntimeError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyList, PyString};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use stemma::api::{
    AuditReport, Diagnostic, ExportOptions, Resolution, ResolveSelectionAction, RevisionRecord,
    RuntimeError, ValidationIssueCode, ValidationReport,
};
use stemma::audit::{
    DirectChange, PreexistingRevision, RevisionDisposition, UntouchedProof, UntouchedViolation,
    UntouchedViolationKind,
};
use stemma::runtime::{
    AmbiguousAnchorDetails, AmbiguousFormatTargetDetails, AuthorLabelCollisionDetails,
    ContainerRevisionDetails, ErrorCode, ErrorDetails, FormatRevisionDetails, FormatTargetDetails,
    FormattingErrorDetails, OpaquePreservationDetails, StaleEditDetails, TypedErrorDetails,
};
use stemma_artifacts::{ArtifactError, PathAuthority};

// ---------------------------------------------------------------------------
// Exceptions
// ---------------------------------------------------------------------------

create_exception!(
    stemma,
    StemmaError,
    PyException,
    "An engine refusal. `code` is the engine's stable refusal name, `message` the \
     actionable explanation, `details` the structured context (a dict). Nothing \
     was changed: every refused call is atomic."
);
create_exception!(
    stemma,
    TransactionError,
    StemmaError,
    "The transaction is not a valid v4 transaction (code `schema_error`) or uses \
     a v4 surface the engine does not route (code `adapter_error`). Raised before \
     the document is consulted."
);
create_exception!(
    stemma,
    StaleEdit,
    StemmaError,
    "The addressed content changed after it was read (`guard` or `expect` \
     mismatch). Re-read the block and rebuild the operation; never retry unchanged."
);
create_exception!(
    stemma,
    NoOpEdit,
    StemmaError,
    "The operation would not change anything (the replacement equals the current \
     content)."
);
create_exception!(
    stemma,
    AmbiguousAnchorAfterMove,
    StemmaError,
    "The operation anchors on a block that a tracked move left as its old-position \
     shadow. `details['ambiguous_anchor']` names the moved copy to anchor on."
);
create_exception!(
    stemma,
    OpaqueDestroyed,
    StemmaError,
    "The operation would drop an opaque object (image, field, content control). \
     `details['opaque_preservation']` lists the ids that would be lost."
);
create_exception!(
    stemma,
    AuthorLabelCollision,
    StemmaError,
    "The tracked write's author label already labels revisions that were pending \
     when the document was opened. Pass `allow_existing_author=True` only if the \
     user intends that existing reviewer group; otherwise ask for another label."
);
create_exception!(
    stemma,
    InvalidRange,
    StemmaError,
    "A revision selection matched nothing (for example an empty id set). Re-read \
     `revisions()` and correct the selection."
);
create_exception!(
    stemma,
    FormatRevisionConflict,
    StemmaError,
    "The run already carries an independently resolvable formatting revision. \
     `details['formatting']` names it."
);
create_exception!(
    stemma,
    AmbiguousFormatTarget,
    StemmaError,
    "A formatting `expect` matched more than one span in the block. Lengthen it \
     until it matches one."
);

/// The exception class for an engine refusal code. Subclasses exist exactly
/// for the eight codes in docs/reference/python.md's exception table; every
/// other code raises the base class with `code` set. Exhaustive on purpose: a new engine code is
/// a compile error here, never a silent mapping.
fn exception_class<'py>(py: Python<'py>, code: &ErrorCode) -> Bound<'py, pyo3::types::PyType> {
    match code {
        ErrorCode::StaleEdit => py.get_type::<StaleEdit>(),
        ErrorCode::NoOpEdit => py.get_type::<NoOpEdit>(),
        ErrorCode::AmbiguousAnchorAfterMove => py.get_type::<AmbiguousAnchorAfterMove>(),
        ErrorCode::OpaqueDestroyed => py.get_type::<OpaqueDestroyed>(),
        ErrorCode::AuthorLabelCollision => py.get_type::<AuthorLabelCollision>(),
        ErrorCode::InvalidRange => py.get_type::<InvalidRange>(),
        ErrorCode::FormatRevisionConflict => py.get_type::<FormatRevisionConflict>(),
        ErrorCode::AmbiguousFormatTarget => py.get_type::<AmbiguousFormatTarget>(),
        ErrorCode::UnsupportedEdit
        | ErrorCode::AnchorNotFound
        | ErrorCode::PrefixDuplicatesLabel
        | ErrorCode::InvalidDocx
        | ErrorCode::InvalidSnapshot
        | ErrorCode::InternalError
        | ErrorCode::ValidationFailed => py.get_type::<StemmaError>(),
    }
}

/// Instantiate `class` and attach the structured fields. If building the
/// exception itself fails, that failure is what propagates.
fn stemma_exception(
    py: Python<'_>,
    class: Bound<'_, pyo3::types::PyType>,
    code: &str,
    message: &str,
    details: &Value,
) -> PyErr {
    let build = || -> PyResult<PyErr> {
        let instance = class.call1((format!("{code}: {message}"),))?;
        instance.setattr("code", code)?;
        instance.setattr("message", message)?;
        instance.setattr("details", to_py(py, details)?)?;
        Ok(PyErr::from_value(instance))
    };
    build().unwrap_or_else(|failure| failure)
}

fn runtime_error(py: Python<'_>, error: RuntimeError) -> PyErr {
    let RuntimeError {
        code,
        message,
        details,
    } = error;
    stemma_exception(
        py,
        exception_class(py, &code),
        &format!("{code:?}"),
        &message,
        &error_details_json(&details),
    )
}

fn transaction_error(py: Python<'_>, code: &str, message: String) -> PyErr {
    stemma_exception(
        py,
        py.get_type::<TransactionError>(),
        code,
        &message,
        &json!({}),
    )
}

/// Host-side file errors become the matching builtin `OSError` subclass, with
/// the artifact boundary's message (which names the path).
fn artifact_error(error: ArtifactError) -> PyErr {
    let message = error.to_string();
    if matches!(error, ArtifactError::OutputExists { .. }) {
        return PyFileExistsError::new_err(message);
    }
    let io_kind = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .map(std::io::Error::kind);
    io_error_class(io_kind, message)
}

fn io_error_class(kind: Option<std::io::ErrorKind>, message: String) -> PyErr {
    use std::io::ErrorKind;
    match kind {
        Some(ErrorKind::NotFound) => PyFileNotFoundError::new_err(message),
        Some(ErrorKind::PermissionDenied) => PyPermissionError::new_err(message),
        Some(ErrorKind::AlreadyExists) => PyFileExistsError::new_err(message),
        Some(ErrorKind::IsADirectory) => PyIsADirectoryError::new_err(message),
        _ => PyOSError::new_err(message),
    }
}

/// A Rust value failed to encode as JSON. Every encoded type is plain data,
/// so this is a binding defect; it fails loud rather than returning a partial
/// value.
fn encode_error(what: &str, error: serde_json::Error) -> PyErr {
    PyRuntimeError::new_err(format!("stemma binding cannot encode {what}: {error}"))
}

// ---------------------------------------------------------------------------
// JSON <-> Python
// ---------------------------------------------------------------------------

fn to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any()
            } else if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else if let Some(f) = n.as_f64() {
                PyFloat::new(py, f).into_any()
            } else {
                return Err(PyRuntimeError::new_err(format!(
                    "stemma binding cannot represent JSON number {n}"
                )));
            }
        }
        Value::String(s) => PyString::new(py, s).into_any(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, item) in map {
                dict.set_item(key, to_py(py, item)?)?;
            }
            dict.into_any()
        }
    })
}

fn to_py_object(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(to_py(py, value)?.unbind())
}

/// A transaction is the v4 JSON text, or a dict that encodes to it. The dict
/// is encoded with the standard library's `json` (NaN/Infinity refused, as the
/// JSON grammar requires); the engine's parser then validates either form.
fn transaction_text(transaction: &Bound<'_, PyAny>) -> PyResult<String> {
    if transaction.is_instance_of::<PyString>() {
        return transaction.extract::<String>();
    }
    if transaction.is_instance_of::<PyDict>() {
        let py = transaction.py();
        let kwargs = PyDict::new(py);
        kwargs.set_item("allow_nan", false)?;
        return py
            .import("json")?
            .call_method("dumps", (transaction,), Some(&kwargs))?
            .extract::<String>();
    }
    Err(PyTypeError::new_err(format!(
        "transaction must be a v4 JSON string or a dict, got {}",
        transaction.get_type().name()?
    )))
}

fn parse_edit_transaction(
    py: Python<'_>,
    transaction: &Bound<'_, PyAny>,
) -> PyResult<stemma::api::EditTransaction> {
    let text = transaction_text(transaction)?;
    let v4 = stemma::edit_v4::parse_transaction(&text)
        .map_err(|e| transaction_error(py, "schema_error", e.to_string()))?;
    v4.into_edit_transaction()
        .map_err(|e| transaction_error(py, "adapter_error", e.to_string()))
}

// ---------------------------------------------------------------------------
// Engine values -> JSON
// ---------------------------------------------------------------------------

fn error_details_json(details: &ErrorDetails) -> Value {
    let ErrorDetails {
        block_id,
        step_index,
        context,
        stale_edit,
        opaque_preservation,
        ambiguous_anchor,
        typed,
    } = details;
    let mut out = serde_json::Map::new();
    if let Some(block_id) = block_id {
        out.insert("block_id".into(), json!(block_id.to_string()));
    }
    if let Some(step_index) = step_index {
        out.insert("step_index".into(), json!(step_index));
    }
    if let Some(context) = context {
        out.insert("context".into(), json!(context));
    }
    if let Some(stale) = stale_edit {
        let value = match stale.as_ref() {
            StaleEditDetails::ExpectMismatch {
                target_block_id,
                expected,
                actual_text,
            } => json!({
                "kind": "expect_mismatch",
                "target_block_id": target_block_id.to_string(),
                "expected": expected,
                "actual_text": actual_text,
            }),
            StaleEditDetails::SemanticHashMismatch {
                target_block_id,
                expected,
                actual,
            } => json!({
                "kind": "semantic_hash_mismatch",
                "target_block_id": target_block_id.to_string(),
                "expected": expected,
                "actual": actual,
            }),
        };
        out.insert("stale_edit".into(), value);
    }
    if let Some(opaque) = opaque_preservation {
        let OpaquePreservationDetails {
            target_block_id,
            missing_opaque_ids,
            missing_inline_kinds,
            original_text_preview,
        } = opaque.as_ref();
        out.insert(
            "opaque_preservation".into(),
            json!({
                "target_block_id": target_block_id.to_string(),
                "missing_opaque_ids": missing_opaque_ids,
                "missing_inline_kinds": missing_inline_kinds,
                "original_text_preview": original_text_preview,
            }),
        );
    }
    if let Some(anchor) = ambiguous_anchor {
        let AmbiguousAnchorDetails {
            anchor_id,
            moved_by_step_index,
            moved_to_block_id,
        } = anchor.as_ref();
        out.insert(
            "ambiguous_anchor".into(),
            json!({
                "anchor_id": anchor_id.to_string(),
                "moved_by_step_index": moved_by_step_index,
                "moved_to_block_id": moved_to_block_id.as_ref().map(ToString::to_string),
            }),
        );
    }
    if let Some(typed) = typed {
        match typed.as_ref() {
            TypedErrorDetails::AuthorLabelCollision(AuthorLabelCollisionDetails {
                author_label,
                existing_revision_count,
            }) => {
                out.insert(
                    "author_label_collision".into(),
                    json!({
                        "author_label": author_label,
                        "existing_revision_count": existing_revision_count,
                    }),
                );
            }
            TypedErrorDetails::Formatting(FormattingErrorDetails::Target(target)) => {
                let FormatTargetDetails {
                    block_id,
                    text,
                    tracking_status,
                    existing_revision,
                    container_revision,
                } = target;
                out.insert(
                    "formatting".into(),
                    json!({
                        "kind": "target",
                        "block_id": block_id.to_string(),
                        "text": text,
                        "tracking_status": tracking_status,
                        "existing_revision": existing_revision.as_ref().map(
                            |FormatRevisionDetails { revision_id, author }| json!({
                                "revision_id": revision_id,
                                "author": author,
                            })
                        ),
                        "container_revision": container_revision.as_ref().map(
                            |ContainerRevisionDetails { revision_id, kind }| json!({
                                "revision_id": revision_id,
                                "kind": kind,
                            })
                        ),
                    }),
                );
            }
            TypedErrorDetails::Formatting(FormattingErrorDetails::Ambiguous(
                AmbiguousFormatTargetDetails {
                    block_id,
                    text,
                    occurrences,
                },
            )) => {
                out.insert(
                    "formatting".into(),
                    json!({
                        "kind": "ambiguous",
                        "block_id": block_id.to_string(),
                        "text": text,
                        "occurrences": occurrences,
                    }),
                );
            }
        }
    }
    Value::Object(out)
}

fn revision_json(record: &RevisionRecord) -> Result<Value, serde_json::Error> {
    let RevisionRecord {
        revision_id,
        wire_id,
        author,
        date,
        kind,
        block_id,
        location,
        excerpt,
    } = record;
    Ok(json!({
        "revision_id": revision_id,
        "wire_id": wire_id,
        "author": author,
        "date": date,
        "kind": kind.as_str(),
        "block_id": block_id.to_string(),
        "location": serde_json::to_value(location)?,
        "excerpt": excerpt,
    }))
}

fn revisions_json(records: &[RevisionRecord]) -> Result<Value, serde_json::Error> {
    records
        .iter()
        .map(revision_json)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn validation_json(report: &ValidationReport) -> Value {
    let ValidationReport { ok, issues } = report;
    let issues: Vec<Value> = issues
        .iter()
        .map(|issue| {
            let code = match issue.code {
                ValidationIssueCode::PackageInvariant => "package_invariant",
                ValidationIssueCode::WordprocessingInvariant => "wordprocessing_invariant",
                ValidationIssueCode::SchemaInvariant => "schema_invariant",
            };
            json!({
                "code": code,
                "message": issue.message,
                "context": issue.context,
            })
        })
        .collect();
    json!({ "ok": ok, "issues": issues })
}

fn audit_json(report: &AuditReport) -> Result<Value, serde_json::Error> {
    let AuditReport {
        new_revisions,
        preexisting_revisions,
        direct_changes,
        untouched,
        validator,
    } = report;

    let preexisting = preexisting_revisions
        .iter()
        .map(
            |PreexistingRevision {
                 record,
                 disposition,
             }| {
                let mut row = revision_json(record)?;
                let (label, after_excerpt) = match disposition {
                    RevisionDisposition::Untouched => ("untouched", None),
                    RevisionDisposition::Modified { after_excerpt } => {
                        ("modified", Some(after_excerpt.as_str()))
                    }
                    RevisionDisposition::Resolved => ("resolved", None),
                };
                row["disposition"] = json!(label);
                row["after_excerpt"] = json!(after_excerpt);
                Ok(row)
            },
        )
        .collect::<Result<Vec<_>, serde_json::Error>>()?;

    let direct = direct_changes
        .iter()
        .map(
            |DirectChange {
                 story,
                 kind,
                 block_id,
                 old_excerpt,
                 new_excerpt,
                 coincides_with_resolution,
             }| {
                Ok(json!({
                    "story": serde_json::to_value(story)?,
                    "kind": kind.as_str(),
                    "block_id": block_id.as_ref().map(ToString::to_string),
                    "old_excerpt": old_excerpt,
                    "new_excerpt": new_excerpt,
                    "coincides_with_resolution": coincides_with_resolution,
                }))
            },
        )
        .collect::<Result<Vec<_>, serde_json::Error>>()?;

    let UntouchedProof {
        verified_blocks,
        parts,
        violations,
    } = untouched;
    let violations = violations
        .iter()
        .map(
            |UntouchedViolation {
                 story,
                 kind,
                 detail,
             }| {
                let mut row = json!({
                    "story": serde_json::to_value(story)?,
                    "detail": detail,
                });
                match kind {
                    UntouchedViolationKind::BlockDiffers {
                        before_block_id,
                        after_block_id,
                    } => {
                        row["kind"] = json!("block_differs");
                        row["before_block_id"] = json!(before_block_id.to_string());
                        row["after_block_id"] = json!(after_block_id.to_string());
                    }
                    UntouchedViolationKind::SequenceLengthMismatch {
                        before_remaining,
                        after_remaining,
                    } => {
                        row["kind"] = json!("sequence_length_mismatch");
                        row["before_remaining"] = json!(before_remaining);
                        row["after_remaining"] = json!(after_remaining);
                    }
                    UntouchedViolationKind::StoryMissing => row["kind"] = json!("story_missing"),
                    UntouchedViolationKind::StoryUnexpected => {
                        row["kind"] = json!("story_unexpected")
                    }
                }
                Ok(row)
            },
        )
        .collect::<Result<Vec<_>, serde_json::Error>>()?;

    Ok(json!({
        "new_revisions": revisions_json(new_revisions)?,
        "preexisting_revisions": preexisting,
        "direct_changes": direct,
        "untouched": {
            "verified_blocks": verified_blocks,
            "parts": parts,
            "violations": violations,
        },
        "validator": validation_json(validator),
    }))
}

fn diagnostics_json(diagnostics: &[Diagnostic]) -> Result<Value, serde_json::Error> {
    serde_json::to_value(diagnostics)
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// An immutable Word document: content plus its tracked changes.
///
/// Every method that changes something returns a NEW `Document`; the receiver
/// is never modified, so earlier states stay valid to read, branch, or save.
/// Construct with `Document.open(path)` or `Document.from_bytes(data)`.
#[pyclass(frozen, module = "stemma")]
struct Document {
    inner: stemma::api::Document,
}

impl Document {
    fn wrap(inner: stemma::api::Document) -> Self {
        Document { inner }
    }
}

#[pymethods]
impl Document {
    /// Parse DOCX bytes (`Document::parse`).
    ///
    /// Raises `StemmaError` (code `InvalidDocx`, …) when the package is not a
    /// DOCX the engine can edit safely. Check `diagnostics()` afterwards: any
    /// normalization applied at import is reported there, never silently.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Document> {
        py.detach(|| stemma::api::Document::parse(data))
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// Read and parse a .docx file. The file is never modified.
    ///
    /// Relative paths resolve against the current directory. Raises the
    /// matching `OSError` subclass when the file cannot be read, and
    /// `StemmaError` when it is not an editable DOCX.
    #[staticmethod]
    fn open(py: Python<'_>, path: PathBuf) -> PyResult<Document> {
        let authority = PathAuthority::explicit().map_err(artifact_error)?;
        let source = authority
            .read_source(&path, "input_docx", None)
            .map_err(artifact_error)?;
        py.detach(|| stemma::api::Document::parse(source.bytes()))
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// Import-time diagnostics (`Document::diagnostics`): every deterministic
    /// normalization applied before this document became editable, as
    /// `{"level": "Info" | "Warning", "message": str, "context": str | None}`.
    /// Carried unchanged by derived documents.
    fn diagnostics(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = diagnostics_json(self.inner.diagnostics())
            .map_err(|e| encode_error("diagnostics", e))?;
        to_py_object(py, &value)
    }

    /// The lean read view (`Document::read`): `{"blocks": [...]}`, one entry
    /// per block in document order with its `id`, `role`, `text`, `guard`,
    /// per-segment tracked status, and more. Address edits with `id` and pin
    /// them with `guard`. The shape is documented in the read model reference
    /// and is engine-version-bound: read it, do not persist it.
    fn read(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let view = py.detach(|| self.inner.read());
        let value = serde_json::to_value(&view).map_err(|e| encode_error("the read view", e))?;
        to_py_object(py, &value)
    }

    /// The extended-markdown reading (`Document::to_markdown`): id-bearing,
    /// revision-aware prose for a model to read. Never parse it to address
    /// edits; use `read()`.
    fn to_markdown(&self, py: Python<'_>) -> String {
        py.detach(|| self.inner.to_markdown())
    }

    /// The plain text of the document as it currently stands
    /// (`Document::to_text`): tracked insertions AND deletions both appear.
    /// For one resolution, project first: `doc.read_accepted().to_text()`.
    fn to_text(&self, py: Python<'_>) -> String {
        py.detach(|| self.inner.to_text())
    }

    /// Apply a v4 transaction (`Document::apply`) and return the new document.
    ///
    /// `transaction` is the v4 JSON text or an equivalent dict, exactly as the
    /// MCP server and CLI accept it (see the v4 operation reference). Tracked
    /// by default. Atomic: on any refusal nothing is applied and the matching
    /// `StemmaError` subclass is raised. This form enforces no author-label
    /// policy; use `apply_authored` when the author comes from a caller.
    fn apply(&self, py: Python<'_>, transaction: &Bound<'_, PyAny>) -> PyResult<Document> {
        let txn = parse_edit_transaction(py, transaction)?;
        py.detach(|| self.inner.apply(&txn))
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// `apply`, but refusing a tracked write whose `revision.author` already
    /// labels revisions that were pending when the document was opened
    /// (`Document::apply_authored`), raising `AuthorLabelCollision`.
    /// `allow_existing_author=True` deliberately continues that reviewer group.
    #[pyo3(signature = (transaction, *, allow_existing_author = false))]
    fn apply_authored(
        &self,
        py: Python<'_>,
        transaction: &Bound<'_, PyAny>,
        allow_existing_author: bool,
    ) -> PyResult<Document> {
        let txn = parse_edit_transaction(py, transaction)?;
        py.detach(|| self.inner.apply_authored(&txn, allow_existing_author))
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// Dry run of `apply` (`Document::check`): raises exactly what `apply`
    /// would raise for this transaction, and returns None when it would apply.
    fn check(&self, py: Python<'_>, transaction: &Bound<'_, PyAny>) -> PyResult<()> {
        let txn = parse_edit_transaction(py, transaction)?;
        py.detach(|| self.inner.check(&txn))
            .map_err(|e| runtime_error(py, e))
    }

    /// Resolve tracked changes (`Document::project`) and return the result.
    ///
    /// `resolution` is one of the engine's `Resolution` shapes:
    /// `"accept_all"`, `"reject_all"`, or `"accept"` / `"reject"` together
    /// with `ids`, the `revision_id`s from `revisions()` to resolve (the rest
    /// stay pending). `ids` is required for `"accept"`/`"reject"` and refused
    /// otherwise. An empty `ids` raises `InvalidRange`.
    #[pyo3(signature = (resolution, ids = None))]
    fn project(
        &self,
        py: Python<'_>,
        resolution: &str,
        ids: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Document> {
        let ids = ids.map(revision_ids).transpose()?;
        let resolution = match (resolution, ids) {
            ("accept_all", None) => Resolution::AcceptAll,
            ("reject_all", None) => Resolution::RejectAll,
            ("accept", Some(ids)) => Resolution::Selective {
                ids,
                action: ResolveSelectionAction::Accept,
            },
            ("reject", Some(ids)) => Resolution::Selective {
                ids,
                action: ResolveSelectionAction::Reject,
            },
            ("accept_all" | "reject_all", Some(_)) => {
                return Err(PyValueError::new_err(format!(
                    "project({resolution:?}) resolves every revision and takes no ids; \
                     use \"accept\" or \"reject\" with ids to resolve a selection"
                )));
            }
            ("accept" | "reject", None) => {
                return Err(PyValueError::new_err(format!(
                    "project({resolution:?}) needs ids: the revision_ids from revisions() to resolve"
                )));
            }
            (other, _) => {
                return Err(PyValueError::new_err(format!(
                    "unknown resolution {other:?}; expected \"accept_all\", \"reject_all\", \
                     \"accept\", or \"reject\""
                )));
            }
        };
        py.detach(|| self.inner.project(resolution))
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// Accept every tracked change (`Document::read_accepted`; the same as
    /// `project("accept_all")`).
    fn read_accepted(&self, py: Python<'_>) -> PyResult<Document> {
        py.detach(|| self.inner.read_accepted())
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// Reject every tracked change (`Document::read_rejected`; the same as
    /// `project("reject_all")`).
    fn read_rejected(&self, py: Python<'_>) -> PyResult<Document> {
        py.detach(|| self.inner.read_rejected())
            .map(Document::wrap)
            .map_err(|e| runtime_error(py, e))
    }

    /// Every pending revision, in document order (`Document::revisions`): the
    /// engine's one census, formatting changes included. Each row has
    /// `revision_id` (the id `project` selects; `0` marks a census-only row
    /// that cannot be selected), `wire_id` (the raw `w:id`, diagnostics only),
    /// `author`, `date`, `kind`, `block_id`, `location` (the story), and
    /// `excerpt`. One revision can have several rows (one per carrier).
    fn revisions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let records = py.detach(|| self.inner.revisions());
        let value = revisions_json(&records).map_err(|e| encode_error("revisions", e))?;
        to_py_object(py, &value)
    }

    /// Audit everything this document changed since it was opened
    /// (`Document::review`), as a dict with `new_revisions`,
    /// `preexisting_revisions` (each with its `disposition`),
    /// `direct_changes` (untracked edits), `untouched` (the proof that
    /// everything else is unchanged), and `validator` (the package verdict on
    /// the would-be saved bytes). Reports; never gates.
    fn review(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let report = py
            .detach(|| self.inner.review())
            .map_err(|e| runtime_error(py, e))?;
        let value = audit_json(&report).map_err(|e| encode_error("the review report", e))?;
        to_py_object(py, &value)
    }

    /// Validated DOCX bytes (`Document::serialize` with the default export
    /// options). Bytes that would violate a structural invariant Word rejects
    /// are refused with `StemmaError` (code `ValidationFailed`), never
    /// returned.
    fn serialize<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py
            .detach(|| self.inner.serialize(&ExportOptions::default()))
            .map_err(|e| runtime_error(py, e))?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Serialize (validated, as `serialize()`) and write to `path`.
    ///
    /// By default the path must not exist: an existing file raises
    /// `FileExistsError` and is left untouched (the CLI's create-new rule).
    /// `overwrite=True` replaces an existing file atomically instead. Returns
    /// `{"path": str, "bytes": int, "sha256": str}` for the written file.
    #[pyo3(signature = (path, *, overwrite = false))]
    fn save(&self, py: Python<'_>, path: PathBuf, overwrite: bool) -> PyResult<Py<PyAny>> {
        let bytes = py
            .detach(|| self.inner.serialize(&ExportOptions::default()))
            .map_err(|e| runtime_error(py, e))?;
        let receipt = if overwrite {
            replace_file(&path, &bytes)?
        } else {
            let authority = PathAuthority::explicit().map_err(artifact_error)?;
            let output = authority
                .commit_new(&path, "output_docx", &bytes, &[])
                .map_err(artifact_error)?;
            json!({
                "path": output.identity.resolved_path.to_string_lossy(),
                "bytes": output.identity.bytes,
                "sha256": output.identity.digest.hex,
            })
        };
        to_py_object(py, &receipt)
    }
}

/// `ids` for a selective `project`: any iterable of revision ids (list,
/// tuple, set, …), each a non-negative int that fits the engine's `u32`.
fn revision_ids(ids: &Bound<'_, PyAny>) -> PyResult<HashSet<u32>> {
    if ids.is_instance_of::<PyString>() {
        return Err(PyTypeError::new_err(
            "ids must be an iterable of revision_id ints, not a string",
        ));
    }
    ids.try_iter()?.map(|id| id?.extract::<u32>()).collect()
}

/// `save(..., overwrite=True)`: stage beside the destination, flush, then
/// rename over it, so a reader sees the old file or the complete new one.
fn replace_file(path: &Path, bytes: &[u8]) -> PyResult<Value> {
    let context = |what: &str, error: std::io::Error| {
        io_error_class(
            Some(error.kind()),
            format!("cannot {what} {}: {error}", path.display()),
        )
    };
    let absolute = std::path::absolute(path).map_err(|e| context("resolve", e))?;
    let parent = absolute.parent().ok_or_else(|| {
        PyValueError::new_err(format!("{} has no parent directory", absolute.display()))
    })?;
    let mut staged = tempfile::Builder::new()
        .prefix(".stemma-save-")
        .tempfile_in(parent)
        .map_err(|e| context("stage a file next to", e))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|e| context("write the staged copy of", e))?;
    staged
        .persist(&absolute)
        .map_err(|e| context("replace", e.error))?;
    Ok(json!({
        "path": absolute.to_string_lossy(),
        "bytes": bytes.len(),
        "sha256": format!("{:x}", Sha256::digest(bytes)),
    }))
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// The result of `compare()` (`stemma_diff::ComparisonResult`).
#[pyclass(frozen, module = "stemma")]
struct Comparison {
    document: Py<Document>,
    semantic_change_count: usize,
    base_diagnostics: Vec<Diagnostic>,
    target_diagnostics: Vec<Diagnostic>,
    flattened_base: Vec<RevisionRecord>,
    flattened_target: Vec<RevisionRecord>,
}

#[pymethods]
impl Comparison {
    /// The redline: an ordinary `Document` whose tracked changes turn the
    /// accepted base into the accepted target. Rejecting all gives the base
    /// reading, accepting all the target reading.
    #[getter]
    fn document(&self, py: Python<'_>) -> Py<Document> {
        self.document.clone_ref(py)
    }

    /// Number of semantic differences found between the accepted inputs.
    #[getter]
    fn semantic_change_count(&self) -> usize {
        self.semantic_change_count
    }

    /// Import diagnostics of the base input (same shape as
    /// `Document.diagnostics()`).
    #[getter]
    fn base_diagnostics(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = diagnostics_json(&self.base_diagnostics)
            .map_err(|e| encode_error("base diagnostics", e))?;
        to_py_object(py, &value)
    }

    /// Import diagnostics of the target input.
    #[getter]
    fn target_diagnostics(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = diagnostics_json(&self.target_diagnostics)
            .map_err(|e| encode_error("target diagnostics", e))?;
        to_py_object(py, &value)
    }

    /// Revisions that were pending in each input and were accepted to obtain
    /// the readings compared: `{"base": [...], "target": [...]}`, rows shaped
    /// like `Document.revisions()`. Empty lists mean the inputs were clean.
    #[getter]
    fn flattened_input_revisions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = json!({
            "base": revisions_json(&self.flattened_base)
                .map_err(|e| encode_error("flattened base revisions", e))?,
            "target": revisions_json(&self.flattened_target)
                .map_err(|e| encode_error("flattened target revisions", e))?,
        });
        to_py_object(py, &value)
    }
}

/// Compare two documents' accepted readings into a tracked-changes redline
/// (`stemma_diff::diff_as_detailed`, or `diff_detailed` when `author` is None).
///
/// `author` labels every discovered revision; None leaves the redline
/// anonymous (an empty author label). An empty string is refused: pass None.
#[pyfunction]
#[pyo3(signature = (base, target, *, author = None))]
fn compare(
    py: Python<'_>,
    base: &Document,
    target: &Document,
    author: Option<&str>,
) -> PyResult<Comparison> {
    let result = py
        .detach(|| match author {
            Some(name) => stemma_diff::diff_as_detailed(&base.inner, &target.inner, name),
            None => stemma_diff::diff_detailed(&base.inner, &target.inner),
        })
        .map_err(|e| runtime_error(py, e))?;
    let stemma_diff::ComparisonResult {
        document,
        semantic_change_count,
        base_diagnostics,
        target_diagnostics,
        flattened_input_revisions,
    } = result;
    let stemma_diff::FlattenedInputRevisions {
        base: flattened_base,
        target: flattened_target,
    } = flattened_input_revisions;
    Ok(Comparison {
        document: Py::new(py, Document::wrap(document))?,
        semantic_change_count,
        base_diagnostics,
        target_diagnostics,
        flattened_base,
        flattened_target,
    })
}

/// Audit any before/after pair of DOCX bytes (`stemma::api::audit`); the
/// edits need not have been made by Stemma. Same report shape as
/// `Document.review()`, with the package verdict computed on `after`.
#[pyfunction]
fn audit(py: Python<'_>, before: &[u8], after: &[u8]) -> PyResult<Py<PyAny>> {
    let report = py
        .detach(|| stemma::api::audit(before, after))
        .map_err(|e| runtime_error(py, e))?;
    let value = audit_json(&report).map_err(|e| encode_error("the audit report", e))?;
    to_py_object(py, &value)
}

/// Validate DOCX bytes as they are, without normalizing them
/// (`stemma::api::validate`): `{"ok": bool, "issues": [{"code", "message",
/// "context"}]}`. Invalid bytes are a result, not an exception.
#[pyfunction]
fn validate(py: Python<'_>, data: &[u8]) -> PyResult<Py<PyAny>> {
    let report = py.detach(|| stemma::api::validate(data));
    to_py_object(py, &validation_json(&report))
}

#[pymodule]
#[pyo3(name = "_native")]
fn native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Document>()?;
    m.add_class::<Comparison>()?;
    m.add_function(wrap_pyfunction!(compare, m)?)?;
    m.add_function(wrap_pyfunction!(audit, m)?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add("StemmaError", py.get_type::<StemmaError>())?;
    m.add("TransactionError", py.get_type::<TransactionError>())?;
    m.add("StaleEdit", py.get_type::<StaleEdit>())?;
    m.add("NoOpEdit", py.get_type::<NoOpEdit>())?;
    m.add(
        "AmbiguousAnchorAfterMove",
        py.get_type::<AmbiguousAnchorAfterMove>(),
    )?;
    m.add("OpaqueDestroyed", py.get_type::<OpaqueDestroyed>())?;
    m.add(
        "AuthorLabelCollision",
        py.get_type::<AuthorLabelCollision>(),
    )?;
    m.add("InvalidRange", py.get_type::<InvalidRange>())?;
    m.add(
        "FormatRevisionConflict",
        py.get_type::<FormatRevisionConflict>(),
    )?;
    m.add(
        "AmbiguousFormatTarget",
        py.get_type::<AmbiguousFormatTarget>(),
    )?;
    Ok(())
}
