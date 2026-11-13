"""Type stubs for the Stemma engine binding (experimental; may change before 1.0).

Every name here is re-exported from ``stemma``. Each method maps to one call on
the Rust facade (``stemma::api::Document``, ``stemma::api::{audit, validate}``,
``stemma_diff::diff[_as]_detailed``); the docstrings name it.

The return and argument shapes (``Transaction`` and the ``TypedDict`` classes)
live in ``stemma._types``, which exists at runtime.
"""

import os
from typing import Any, Iterable, Literal, Optional, Union, final

from ._types import (
    Diagnostic,
    FlattenedInputRevisions,
    Revision,
    SaveReceipt,
    Transaction,
    ValidationIssue,
    ValidationReport,
)

__all__ = [
    "AmbiguousAnchorAfterMove",
    "AmbiguousFormatTarget",
    "AuthorLabelCollision",
    "Comparison",
    "Document",
    "FormatRevisionConflict",
    "InvalidRange",
    "NoOpEdit",
    "OpaqueDestroyed",
    "StaleEdit",
    "StemmaError",
    "TransactionError",
    "__version__",
    "audit",
    "compare",
    "validate",
]
__version__: str

class StemmaError(Exception):
    """An engine refusal. Nothing was changed: every refused call is atomic.

    ``code`` is the engine's stable refusal name (``"StaleEdit"``,
    ``"ValidationFailed"``, …; ``"schema_error"``/``"adapter_error"`` for
    ``TransactionError``). ``message`` is the actionable explanation.
    ``details`` is structured context; keys present depending on the refusal:
    ``block_id``, ``step_index``, ``context``, ``stale_edit``,
    ``opaque_preservation``, ``ambiguous_anchor``, ``author_label_collision``,
    ``formatting``.
    """

    code: str
    message: str
    details: dict[str, Any]

class TransactionError(StemmaError):
    """Not a valid v4 transaction (``schema_error``), or a v4 surface the engine does not route (``adapter_error``)."""

class StaleEdit(StemmaError):
    """The addressed content changed after it was read. Re-read and rebuild; never retry unchanged."""

class NoOpEdit(StemmaError):
    """The operation would not change anything."""

class AmbiguousAnchorAfterMove(StemmaError):
    """The anchor is a tracked move's old-position shadow; ``details["ambiguous_anchor"]`` names the moved copy."""

class OpaqueDestroyed(StemmaError):
    """The operation would drop an opaque object; ``details["opaque_preservation"]`` lists the ids."""

class AuthorLabelCollision(StemmaError):
    """The author label already labels revisions pending at open. Pass ``allow_existing_author=True`` only if intended."""

class InvalidRange(StemmaError):
    """A revision selection matched nothing (e.g. empty ids)."""

class FormatRevisionConflict(StemmaError):
    """The run already carries a resolvable formatting revision; ``details["formatting"]`` names it."""

class AmbiguousFormatTarget(StemmaError):
    """A formatting ``expect`` matched more than one span; lengthen it."""

@final
class Document:
    """An immutable Word document: content plus its tracked changes.

    Methods that change something return a NEW ``Document``. Construct with
    ``Document.open(path)`` or ``Document.from_bytes(data)``.
    """

    @staticmethod
    def from_bytes(data: Union[bytes, bytearray, memoryview]) -> Document:
        """Parse DOCX bytes (``Document::parse``). Raises ``StemmaError`` (e.g. ``InvalidDocx``)."""
    @staticmethod
    def open(path: Union[str, os.PathLike[str]]) -> Document:
        """Read and parse a .docx file; never modifies it. ``OSError`` subclasses for file errors."""
    def diagnostics(self) -> list[Diagnostic]:
        """Import-time normalizations (``Document::diagnostics``). Carried by derived documents."""
    def read(self) -> dict[str, Any]:
        """The lean read view (``Document::read``): ``{"blocks": [...]}``.

        Each block carries ``id`` (the edit target), ``guard`` (pins the edit
        to what you read), ``role``, ``text``, and its tracked segments. The
        shape is engine-version-bound: read it, do not persist it.
        """
    def to_markdown(self) -> str:
        """Id-bearing, revision-aware prose for a model to read (``Document::to_markdown``)."""
    def to_text(self) -> str:
        """Plain text as the document stands; insertions and deletions both appear (``Document::to_text``)."""
    def apply(self, transaction: Transaction) -> Document:
        """Apply a v4 transaction (``Document::apply``); tracked by default, atomic, no author-label policy."""
    def apply_authored(
        self, transaction: Transaction, *, allow_existing_author: bool = False
    ) -> Document:
        """``apply`` that raises ``AuthorLabelCollision`` for an author label pending at open (``Document::apply_authored``)."""
    def check(self, transaction: Transaction) -> None:
        """Dry run of ``apply`` (``Document::check``): raises exactly what ``apply`` would."""
    def project(
        self,
        resolution: Literal["accept_all", "reject_all", "accept", "reject"],
        ids: Optional[Iterable[int]] = None,
    ) -> Document:
        """Resolve tracked changes (``Document::project``).

        ``"accept_all"`` / ``"reject_all"`` take no ids. ``"accept"`` /
        ``"reject"`` require ``ids``, revision_ids from ``revisions()``; the
        rest stay pending. Empty ids raise ``InvalidRange``.
        """
    def read_accepted(self) -> Document:
        """Accept every tracked change (``Document::read_accepted``)."""
    def read_rejected(self) -> Document:
        """Reject every tracked change (``Document::read_rejected``)."""
    def revisions(self) -> list[Revision]:
        """Every pending revision in document order (``Document::revisions``); one row per carrier."""
    def review(self) -> dict[str, Any]:
        """Audit everything changed since open (``Document::review``).

        Keys: ``new_revisions``, ``preexisting_revisions`` (rows with
        ``disposition`` and ``after_excerpt``), ``direct_changes`` (untracked
        edits), ``untouched`` (``verified_blocks``, ``parts``,
        ``violations``), ``validator`` (a ``ValidationReport``).
        """
    def serialize(self) -> bytes:
        """Validated DOCX bytes (``Document::serialize``); ``StemmaError`` ``ValidationFailed`` if Word would reject them."""
    def save(
        self, path: Union[str, os.PathLike[str]], *, overwrite: bool = False
    ) -> SaveReceipt:
        """Serialize and write. Existing path: ``FileExistsError`` unless ``overwrite=True`` (atomic replace)."""

@final
class Comparison:
    """Result of ``compare()`` (``stemma_diff::ComparisonResult``)."""

    @property
    def document(self) -> Document:
        """The redline: reject-all gives the base reading, accept-all the target reading."""
    @property
    def semantic_change_count(self) -> int:
        """Semantic differences found between the accepted inputs."""
    @property
    def base_diagnostics(self) -> list[Diagnostic]: ...
    @property
    def target_diagnostics(self) -> list[Diagnostic]: ...
    @property
    def flattened_input_revisions(self) -> FlattenedInputRevisions:
        """Pending input revisions accepted to obtain the compared readings."""

def compare(
    base: Document, target: Document, *, author: Optional[str] = None
) -> Comparison:
    """Compare accepted readings into a redline (``stemma_diff::diff_as_detailed``; ``diff_detailed`` when author is None).

    An empty ``author`` is refused; pass None for an anonymous redline.
    """

def audit(
    before: Union[bytes, bytearray, memoryview], after: Union[bytes, bytearray, memoryview]
) -> dict[str, Any]:
    """Audit any before/after DOCX pair (``stemma::api::audit``). Same shape as ``Document.review()``."""

def validate(data: Union[bytes, bytearray, memoryview]) -> ValidationReport:
    """Validate DOCX bytes as they are (``stemma::api::validate``). Invalid bytes are a result, not an exception."""
