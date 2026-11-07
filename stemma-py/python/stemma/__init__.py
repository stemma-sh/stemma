"""Stemma: edit existing Word documents with native tracked changes.

Experimental: the Python API may change before 1.0. See the Python reference
in the Stemma documentation: https://stemma.sh/docs/reference/python

The package is a thin binding over the Rust engine's ``api::Document`` facade:
transactions go in as v4 JSON (a string or a dict), results come back as plain
Python values. Every ``Document`` is immutable; methods that change something
return a new ``Document``.
"""

from ._native import (
    AmbiguousAnchorAfterMove,
    AmbiguousFormatTarget,
    AuthorLabelCollision,
    Comparison,
    Document,
    FormatRevisionConflict,
    InvalidRange,
    NoOpEdit,
    OpaqueDestroyed,
    StaleEdit,
    StemmaError,
    TransactionError,
    __version__,
    audit,
    compare,
    validate,
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
