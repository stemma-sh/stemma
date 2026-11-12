"""Return and argument shapes of the Stemma binding, for type checkers.

They are ordinary runtime objects (importable from ``stemma``), but the
binding returns plain ``dict`` and ``list`` values: a ``TypedDict`` describes
a shape, it is not a class the binding instantiates.
"""

from typing import Any, Literal, Optional, TypedDict, Union

Transaction = Union[str, dict[str, Any]]
"""A v4 transaction: the JSON text, or a dict that encodes to it."""

class Diagnostic(TypedDict):
    level: Literal["Info", "Warning"]
    message: str
    context: Optional[str]

class Revision(TypedDict):
    revision_id: int
    """Engine identity that ``Document.project`` selects; 0 = census-only, not selectable."""
    wire_id: int
    """The raw OOXML ``w:id``; diagnostics only, never an address."""
    author: Optional[str]
    date: Optional[str]
    kind: Literal[
        "insert",
        "delete",
        "format_run",
        "format_paragraph",
        "format_paragraph_mark",
        "format_table",
        "format_row",
        "format_cell",
        "format_section",
        "opaque_interior",
        "move",
        "cell_merge",
    ]
    block_id: str
    location: Any
    """The story: ``"Body"``, or e.g. ``{"Footnote": {"id": "2"}}``."""
    excerpt: str

class ValidationIssue(TypedDict):
    code: Literal["package_invariant", "wordprocessing_invariant", "schema_invariant"]
    message: str
    context: Optional[str]

class ValidationReport(TypedDict):
    ok: bool
    issues: list[ValidationIssue]

class SaveReceipt(TypedDict):
    path: str
    bytes: int
    sha256: str

class FlattenedInputRevisions(TypedDict):
    base: list[Revision]
    target: list[Revision]
