# 60-second approved-worklist demo

This synthetic walkthrough uses a prebuilt `stemma` executable, `jq`, and the
repository fixtures. It demonstrates only the experimental top-level
body-paragraph workflow.

| Time | Show | Narration |
|---:|---|---|
| 0-10s | Extract the input text and display the one-item worklist. | Approval is bound to the exact input bytes and one expected match. |
| 10-25s | Run `stemma apply`; show exit `0`, `stemma validate`, and the minimized receipt summary. | Complete means every item applied, the output is deliverable, and the persisted size/hash tuple agrees. |
| 25-38s | Show the redline's native revision inventory. | The output is a new DOCX with a tracked delete and insert by the declared reviewer; the original stays unchanged. |
| 38-55s | Run the missing-text worklist; show exit `3`, its partial receipt, and absence of `refused.docx`. | Ambiguity or mismatch is refused rather than guessed. A safe refusal is not a completed redline. |
| 55-60s | Point to the CLI contract. | Tables and non-body stories are outside this experimental v0 boundary. |

Run the exact sequence:

```bash
STEMMA=/path/to/stemma ./scripts/demo-approved-worklist.sh
```

The script fails immediately if the refusal exit is not `3`, if its receipt is
missing, or if a refused DOCX appears. It uses a temporary directory and removes
the synthetic outputs on exit. Full semantics and receipt fields are in the
[CLI reference](reference/cli.md).
