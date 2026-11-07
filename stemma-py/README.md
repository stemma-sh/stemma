# stemma-py

Edit existing Word documents from Python with native tracked changes, the way
Word itself represents them. A reviewer opens the result in Word and accepts or
rejects each change.

> **Experimental.** The Python API may change before 1.0. The engine underneath
> is the same one the Stemma CLI and MCP server run.

```bash
pip install stemma-py
```

The PyPI name is `stemma-py`; the import name is `stemma`. Wheels are
self-contained: no compiler and no Python dependencies.

```python
import stemma

doc = stemma.Document.open("contract.docx")
block = doc.read()["blocks"][1]

edited = doc.apply_authored({
    "ops": [{
        "op": "replace",
        "target": block["id"],
        "guard": block["guard"],
        "content": {"type": "paragraph",
                    "content": [{"type": "text", "text": "Liability is capped at twice the fees paid."}]},
    }],
    "revision": {"author": "J. Osei"},
})

assert edited.review()["validator"]["ok"]
edited.save("contract-redline.docx")
```

Everything else (comparing two versions, resolving revisions, auditing a
delivery, the error classes) is in the
[Python reference](https://stemma.sh/docs/reference/python).

Licensed under either of MIT or Apache-2.0, at your option.
