"""Published Python snippets run against the package.

docs/reference/python.md and the two READMEs are what an integrator copies
from; these tests keep them from drifting away from the binding. A page's
snippets share one namespace (later ones use earlier results) and run in a
scratch directory holding the files they name.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from test_binding import NEW_TEXT, make_docx

REPO = Path(__file__).resolve().parents[2]
SNIPPET = re.compile(r"^```python\n(.*?)^```$", re.MULTILINE | re.DOTALL)


def run_snippets(page: Path) -> dict[str, object]:
    snippets = SNIPPET.findall(page.read_text(encoding="utf-8"))
    assert snippets, f"no Python snippets found in {page}"
    namespace: dict[str, object] = {"__name__": "docs_snippets"}
    for index, snippet in enumerate(snippets):
        exec(compile(snippet, f"{page.name} snippet {index + 1}", "exec"), namespace)
    return namespace


def test_reference_page_snippets_run_in_order(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    original = make_docx("Definitions.", "Liability is capped at the fees paid.")
    (tmp_path / "contract.docx").write_bytes(original)
    (tmp_path / "as-sent.docx").write_bytes(original)
    (tmp_path / "as-returned.docx").write_bytes(make_docx("Definitions.", NEW_TEXT))
    monkeypatch.chdir(tmp_path)

    namespace = run_snippets(REPO / "docs" / "reference" / "python.md")

    assert (tmp_path / "contract-redline.docx").is_file()
    assert (tmp_path / "changed.docx").is_file()
    # The error snippet's `except stemma.StaleEdit` branch ran.
    assert "current" in namespace


@pytest.mark.parametrize(
    "readme, source_name, source_text",
    [
        ("README.md", "agreement.docx", "Payment is due within 30 days."),
        ("stemma-py/README.md", "contract.docx", "Liability is capped at the fees paid."),
    ],
)
def test_readme_snippet_runs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, readme: str, source_name: str, source_text: str
) -> None:
    import stemma

    blocks = ("Definitions.", source_text) if source_name == "contract.docx" else (source_text,)
    (tmp_path / source_name).write_bytes(make_docx(*blocks))
    monkeypatch.chdir(tmp_path)

    run_snippets(REPO / readme)

    outputs = sorted(p.name for p in tmp_path.iterdir() if p.name != source_name)
    assert len(outputs) == 1, outputs
    redline = stemma.Document.open(tmp_path / outputs[0])
    assert redline.read_rejected().to_text() == stemma.Document.open(tmp_path / source_name).to_text()
    assert redline.revisions()
