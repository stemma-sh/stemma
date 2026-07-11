# Agent benchmarks: stemma-mcp vs. the stock DOCX skill

**TL;DR.** Claude's stock DOCX skill is better than its reputation: on
flat-text edits a frontier agent hand-editing the XML matches stemma, and the
edit-safety head-to-head on real dirty documents is a tie. What survives
replication is task-shaped. Suite-wide on frontier models stemma holds
**95%** vs 76–85% for hand-editing — the gap concentrated exactly where
documents stop being flat text (several 3/3-vs-0/3 lanes: stacked redlines,
paragraph-mark joins, table interiors, footnote stories), and the
hand-editing failures are content corruption, not refusals. stemma moves
roughly half the tokens at 2–3× lower latency, and stays flat as documents
grow where raw XML scales with them. One finding worth singling out: the
guardrails do the most for the weakest models — behind the engine Haiku
holds 76% (near-perfect on single-intent operations, at ~$0.10–0.25 per
document) where raw-XML Haiku collapses to 20%. That supports a deployment
split: a frontier model to plan, a cheap fast tier to execute decomposed
edits behind the engine — the interface does not make a small model a
planner (compound multi-step lanes still need the frontier tier). Several
early "moat" claims did not survive our own replication and one flipped to
a loss — all disclosed below.

This is an aggregate, methodology-first report of stemma's agent-layer
benchmarks: what happens when a cold LLM agent is given a Word-document task
and one of two tool surfaces. It includes the cells stemma loses and the
cells that dissolved into ties under replication, because those are what make
the rest of the numbers credible.

**What this measures:** the *interface* — whether an agent driving stemma's
MCP server produces correct tracked-change output, at what cost, compared to
the same agent hand-editing OOXML or driving a competing document MCP.

**What this does NOT measure:** engine correctness. That claim is carried by
the engine's spec-test suite (~1,060 ECMA-376/ISO-29500 conformance tests),
fuzzing, corpus sweeps, and a real-Word conformance tier — not by agent runs.
Do not read any table below as engine validation.

## Methodology

### Arms

Every lane runs the same frozen prompt on the same fixture across two arms:

| Arm | Surface | Setup |
|---|---|---|
| **stemma** | `stemma-mcp` over stdio | agent may call only the stemma MCP tools; the stemma skill document is appended to the system prompt |
| **vanilla** | **Claude's stock DOCX skill** — raw OOXML editing | no stemma; the same agent with `Bash`/`Read`/`Edit`/`Write`/`Glob`/`Grep` plus the official DOCX-editing skill (unzip the package, edit `word/document.xml` directly). Wherever this document says *vanilla*, it means this. |

### Harness and pins

Each trial is a **cold, headless `claude -p` session**: fresh context,
`--max-turns 50`, full `stream-json` transcript captured. Nothing is shared
between runs.

Every cell pins three things — the **model**, the **engine build**, and the
**agent CLI version** — and no cell mixes any of them. All three move the
numbers; CLI drift alone measurably shifts economics (see Reproducibility).
The main table below is the most recent full sweep (Sonnet 5, 2026-07-03/04);
earlier and cheaper pins are compared in the model-tier section, never merged
into it. Sweeps deliberately include a cheap tier alongside the frontier pin —
a weaker model leans harder on what the tool surface gives it, which is
exactly the thing under test. Cost figures are the `claude` CLI's reported
per-run API cost.

### Grading

**Grading is deterministic and engine-driven; the agent's own report is never
trusted.** Each lane ships a frozen fixture (pinned by hash), a frozen prompt,
and a gate script that inspects the *output `.docx`* — the serialized markup
and canonical text projections — against ground truth frozen from the fixture
before any run. Gates are tiered MUST / MAY / MUST-NOT; a run passes its cell
only if every gate passes. Several gates additionally used **a real Microsoft
Word instance as a behavioral oracle** (does the output open with no repair
prompt; do accept-all/reject-all in Word agree with the engine).

Suite-wide invariants, gated on every lane:

- the input file is byte-identical after the run (originals are never touched);
- output is written to a new path and passes the engine's validator; every
  graded output of waves 1–3 and the v2.2 re-runs was additionally opened in
  real Word — see the
  [clean-open record](benchmarks-history.md#word-clean-open-real-word-all-outputs-all-waves);
- all agent-authored revisions carry a single consistent author identity,
  distinct from any pre-existing reviewer;
- no untracked mutations in any task that requests tracked changes.

### Replication policy

Agent runs are stochastic, and this suite's own history proves single runs
mislead (see "What replication overturned" below). The policy:

- **n ≥ 5** for headline claims; **n ≥ 3** for any published pass-rate;
- anything at n < 3 is labeled a **directional observation**, not a claim;
- cells aggregate **all** replication runs — never a best-of-N;
- economics is reported as median with min–max spread;
- every stemma-arm failure gets per-run forensics before publication;
  anything reclassified by forensics is disclosed under "Corrections after
  publication".

## The benchmark

The main table: **Sonnet 5**, pinned wave 2026-07-03/04, engine
`46206e9`/`880c49a`, n=3 per cell. **stemma** = a cold headless agent
restricted to the stemma-mcp tools; **vanilla** = the same agent using
Claude's stock DOCX skill (file tools; it unzips the package and edits the
XML directly). Success = runs passing every gate (no-output runs count as
failures); latency is the per-run median; the last column is stemma's median
token traffic as a fraction of vanilla's (0.45× = stemma moved 45% of the
tokens vanilla needed; tokens count everything that transits the context, so
the ratio is pricing-independent). The document-size and revision-density
lanes are variants of the same tasks, run and graded exactly like every
other row. Every number is machine-checkable against the
[per-cell data](benchmark-data-model-sweeps-2026-07.json).

| task | stemma success | vanilla success | stemma latency | vanilla latency | stemma tokens ÷ vanilla |
|---|---|---|---|---|---|
| stacked-revision resolution | 3/3 | 1/3 | 42s | 272s | 0.16× |
| cascading resolution | 3/3 | 3/3 | 29s | 78s | 0.50× |
| paragraph-join resolution (§17.13.5.15) | 3/3 | 0/3 | 26s | 37s | 0.94× |
| table-interior revision resolution | 3/3 | 0/3 | 25s | 49s | 0.73× |
| paragraph-formatting revision (pPrChange) | 3/3 | 3/3 | 23s | 75s | 0.47× |
| table/cell formatting-change selective resolution | 3/3 | 3/3 | 30s | 109s | 0.35× |
| tracked table-row add + delete | 3/3 | 3/3 | 24s | 42s | 0.92× |
| tracked whole-section delete, renumbering | 3/3 | 3/3 | 20s | 42s | 0.75× |
| tracked bold on defined terms (rPrChange) | 3/3 | 3/3 | 42s | 48s | 1.43× |
| flatten a redline to a clean final | 3/3 | 0/3 | 24s | 78s | 0.46× |
| compare base vs revised into a redline | 3/3 | 3/4 (1 DNF) | 20s | 342s | 0.11× |
| NDA end-to-end edit | 2/3 | 3/3 | 178s | 210s | 0.82× |
| selective accept/reject by author | 3/3 | 3/3 | 33s | 222s | 0.15× |
| policy-manual multi-edit | 3/3 | 3/4 (1 DNF) | 266s | 457s | 0.46× |
| product-spec edit | 3/3 | 2/3 | 111s | 138s | 0.62× |
| tracked clause authoring | 3/3 | 3/3 | 17s | 35s | 0.70× |
| edit inside an existing redline | 3/3 | 3/3 | 36s | 82s | 0.99× |
| refusal / no-corruption under ambiguity | 3/3 | 2/3 (1 DNF) | 402s | 432s | 0.71× |
| add a comment | 3/3 | 3/3 | 19s | 28s | 0.76× |
| insert an image with caption | 2/3 | 3/3 | 163s | 70s | 1.44× |
| insert a native ToC field | 3/3 | 3/3 | 16s | 171s | 0.19× |
| tracked edit inside a footnote body | 3/3 | 3/3 | 21s | 19s | 1.52× |
| selective resolution @ ~300 markers | 3/3 | 3/3 | 76s | 308s | 0.51× |
| @ ~1,000 markers | 3/3 | 2/3 (1 DNF) | 105s | 322s | 0.48× |
| @ ~3,050 markers | 2/3 (1 DNF) | 3/3 | 153s | 341s | 0.89× |
| negotiation loop: selective resolve + tracked counter + comment | 3/3 | 3/3 | 108s | 101s | 1.03× |
| nested revisions: accept A, reject B-inside-A | 3/3 | 3/3 | 43s | 84s | 0.78× |
| negotiation loop in a ~50-page agreement | 3/3 | 3/3 | 132s | 205s | 0.68× |
| negotiation loop in a ~150-page agreement | 2/3 | 3/3 | 144s | 261s | 0.45× |

Reading the table honestly, in both directions:

- The vanilla failures are content corruption, not refusals: dropping a
  table row's text (res7), losing a paragraph in a tracked paragraph-mark
  join (res6 — a lane the previous model pin passed 3/3; stronger ≠ safer
  for hand-editing), accepting formatting that was meant to be reverted.
- stemma's three dropped reps are disclosed and real: one agent stopped
  mid-task to ask a question no headless harness can answer (f1), one
  wedged itself trying a shell route its tool policy blocks (img-1), one
  hit the turn ceiling at the 3,050-marker density (scale-d3).
- On lanes where the task is text-shaped, the vanilla arm is competitive
  and sometimes cheaper — the advantage is task-shaped, and it concentrates
  exactly where documents stop being flat text.
- Where the vanilla arm does pass a heavy lane, it pays for it in turns:
  7–10 stemma turns vs 27–47 vanilla turns on the compare/triage/density
  lanes, which is what drives the cost column.
- The two negotiation-loop size rows are the identical task — the same 43
  tracked changes by two authors, same prompt, targets placed ≥75% deep —
  in a ~50-page and a ~150-page agreement, so document size is the only
  variable. Tripling the document moved stemma by one turn and its token
  traffic did not grow at all (1.77M → 1.57M median tokens: the document
  stays server-side, and the agent's traffic — outline, targeted reads,
  receipts — is size-invariant). The vanilla arm must move the XML through
  the context window and scales with it: +33% tokens and +9 turns for the
  same 43 changes, its median 150-page run at 46 of 50 allowed turns, one
  rung from the ceiling. (stemma's one dropped rep at 150 pages was an
  engine refusal fixed the same week — see the repo history for
  `comment_create` tracked-anchor support; the runs predate the fix.) The
  engine itself resolves the 150-page redline in under two seconds — every
  second in the table is agent loop, not engine.

## Across model tiers

The same suite has been swept on three model pins. The **22 lanes for which
every cell exists** on all three pins, both arms (three vanilla cells never
ran on the Sonnet 4.6 pin; those lanes are excluded from ALL rows so the
rows compare to each other). Aggregation: pass rates pool every replication
run; latency/cost/tokens are medians of per-lane medians; *tokens* =
everything that transited the model's context per run (input + output +
cache creation + cache reads). One caveat the table cannot remove: the
Sonnet 4.6 cells are the historical replicated runs on June-era engine
builds, so that row compares model *and* engine era; the Sonnet 5 and Haiku
rows share a single pinned engine. Full per-cell data:
[`benchmark-data-model-sweeps-2026-07.json`](benchmark-data-model-sweeps-2026-07.json).

| model | arm | pass | latency | $ / task | tokens / task |
|---|---|---|---|---|---|
| Sonnet 4.6 | stemma | **95%** (74/78) | 72s | $0.29 | 356k |
| Sonnet 4.6 | vanilla | 85% (66/78) | 188s | $0.61 | 592k (1.7×) |
| Sonnet 5 | stemma | **95%** (63/66) | **35s** | $0.49 | 469k |
| Sonnet 5 | vanilla | 76% (52/68) | 95s | $0.66 | 1,049k (2.2×) |
| Haiku 4.5 | stemma | 76% (50/66) | 56s | $0.23 | 306k |
| Haiku 4.5 | vanilla | 20% (13/66) | 58s | $0.13 | 514k (1.7×) |

What the rows say together: behind the engine, correctness holds at 95% on
both Sonnet tiers while latency halves on the newer one — the engine
answers in milliseconds, the seconds are agent turns, and a better model
needs fewer of them. Hand-editing never reaches 90% on any tier, pushes
1.7–2.2× the tokens through the context window for the same tasks, and
collapses on the cheap tier (its low Haiku latency is failing fast, not
succeeding fast).

Two Haiku-specific readings matter for deployment. Hand-editing OOXML
collapses without a frontier model: Haiku's raw-XML arm fails even lanes it
aced on Sonnet 5, while behind the engine Haiku stays at or near 3/3 on the
entire resolution family at roughly $0.10–0.25 per document. And the
interface does not make a small model a large one: Haiku went 0/3 on every
compound do-five-things-in-one-pass task while staying near-perfect on
single-intent operations — cheap models are an operational tier for
decomposed calls, not planners. Several Haiku tool-arm cells were
additionally depressed by a harness artifact, the invocation barrier
described under "Corrections after publication".

Per-lane tables for the Sonnet 4.6 and Haiku 4.5 pins, with full reading
notes, are in the [benchmark archive](benchmarks-history.md).

## Robustness on real-world documents

### Real-world corpus roundtrip (n=340)

The lanes above use curated fixtures. The "doesn't corrupt the real world"
property was measured separately: 340 real-world `.docx` (2 KB–1.5 MB, median
36 KB; public filings, OSS test corpora, and a local real-world stress set),
each run through stemma
import → re-serialize → real-Word open:

| stage | result |
|---|---|
| import | 339/340 (99.7%) |
| re-serialize | 338/340 |
| Word clean-open of the re-serialized docs | 335/338 (0 invalid, 3 repaired) |
| **end-to-end** | **335/340 (98.5%)** |

Every failure is **loud** — an explicit refusal with an error code (one
import refusal on an unmodeled element; one serialize refusal caught by
stemma's own pre-write validator; three Word repairs, two of them on
pre-existing redline samples produced by a different legacy tool). Nothing
silently corrupted. Caveat: a raw-XML "roundtrip" is a byte copy, so this
measures stemma's semantic-model robustness at scale, not a head-to-head.

### Edit-path hazards on real documents

Because the corpus roundtrip is not a head-to-head, the edit path was also
measured agent-vs-agent on three hazard classes drawn from real dirty
documents, graded on serialized markup plus real-Word accept/resolve legs:

| hazard | n | result |
|---|---|---|
| target word split across multiple runs | 20 docs | tie — both arms 20/20 Word-clean, correct tracked edits |
| edit inside another author's tracked insertion | 10 docs | tie — 0 author-forges, 0 cascade failures, both arms |
| edit adjacent to equations/drawings/OLE | 10 docs | tie — 0 opaque objects dropped, either arm (incl. an 18-equation doc); stemma declined 2 degenerate targets (word inside a drawing text box) where vanilla edited — a reach limit, not a loss |

**A capable vanilla agent matches stemma on edit safety.** There is no
edit-path corruption/forge/silent-loss moat against a careful raw-XML agent
in these regimes. The differences that remain are: stemma *enforces* these
properties at the write surface (refusals, validation before bytes are
written) where the vanilla agent merely *happened to get them right*, and
stemma does the same work in ~4 tool calls where the vanilla agent hand-edits
across ~26 turns.

## What replication overturned

Read this section before quoting any number above.

The first pass over these lanes was one run per cell, and it looked like a
sweep: five "moat" lanes where stemma passed gates a competitor failed.
Replication at ≥5× dissolved most of it, and we publish that on purpose.
Two "moat" cells flipped to ties when the competitor simply succeeded on
re-run (auth6, safe6); one flipped to a **loss** (f4 — vanilla 5/5, stemma
3/5); one dissolved on re-grading against frozen ground truth (f5 — a tie);
an early Word-repair "headline" did not replicate (35/35 clean, both arms);
several stemma pass-rates turned out flippy run-to-run (run-to-run variance,
not a moat, is the dominant signal there); and one claim died *after* first
publication as a grading artifact (see "Corrections after publication").
The per-lane record behind each of these is in the
[benchmark archive](benchmarks-history.md).

What survives:

1. **Roundtrip fidelity and story-aware flattening are replicated quality
   wins.** res7 (accept-all + reject-all copies of a redline): stemma 5/5
   vs vanilla 2/4 in wave 1 — and 3/3 vs 0/3 again on the Sonnet 5 pin.
   cmp-f1 (flatten a redline incl. footnote stories): stemma 3/3 vs
   vanilla 0/3, where every vanilla run silently emptied the footnote
   text. Two replicated losses in the other direction (tbl-s1, fn-a1) are
   reported with the same weight; both, plus toc-1, were subsequently
   closed by disclosed post-fix re-runs — the loss cells stand in the
   archive as the pre-fix baseline.
2. **Economics: stemma is cheaper on most lanes, and the gap is
   task-shaped.** ~4–8× cheaper where the task forces bulk output through
   the context window (full triage, producing whole documents), near
   cost-parity on surgical single-edit lanes. Qualifiers: (a) the
   comparison is confounded — different surfaces push different volumes of
   XML through the context, so this partly measures token flow, not
   intelligence; (b) it erodes as models and harnesses improve — the wave
   record demonstrates that erosion in the data. One replicated
   counterexample: auth6, where vanilla is ~2× cheaper.
3. **Ingest robustness at scale** (98.5% end-to-end Word-clean, fail-loud on
   the rest) — a property raw-XML editing does not even have a notion of.
4. **Enforced guarantees vs. best-effort:** refusing author impersonation,
   refusing silent destruction of opaque objects, validating every output
   before write. In these runs competitors did not commit those failures —
   the claim is "guaranteed vs. usually right", not "we don't corrupt and
   they do".

## Benchmark-awareness contamination

Publishing this benchmark forced a transcript audit of every graded run, and
the audit found a real flaw: the June harness launched agents with their
working directory *inside the benchmark tree*, and agents with file tools
used that. Two "vanilla" runs located the stemma engine on the host and
**drove it to produce their output**; both are disqualified (they measure
stemma-with-extra-steps, not raw-XML editing), and every corrected cell
shows the clean denominator. One lane's frozen prompt literally named the
stemma plugin — a structurally invalid instruction for a no-stemma arm —
so that lane's vanilla economics are withdrawn entirely. Runs that merely
*read* benchmark internals were kept but are flagged in the results
inventory. The harness now launches agents from a neutral temp directory,
and the transcript audit (harness-read and foreign-tool-execution scans) is
a permanent part of the aggregation pipeline. Full narrative in the
[archive](benchmarks-history.md).

One observation worth keeping: given ordinary file tools, a capable agent
*will* find and use the strongest tool on the host rather than do the task
the hard way. That is a deployment insight — and exactly why benchmark arms
need isolation.

## Corrections after publication

Kept here permanently so nobody has to diff git history to learn them:

- **"Vanilla 0/5 at formatting-change resolution" — withdrawn.** The gate
  was string-comparing serialized XML against ground truth frozen from
  stemma's own serializer, so it graded the serialization dialect (a
  whitespace-only self-closing-tag diff), not the resolution — and stemma
  "passed" because it *is* that serializer. That is the
  teaching-to-the-test failure mode the caveats section warns about,
  caught in our own grading. The gate now canonicalizes fragments before
  comparing; every affected run was re-graded (vanilla 5/5 on the lane);
  no other gate compares raw serialized fragments (audited). Full
  narrative in the [archive](benchmarks-history.md).
- **One Haiku cell re-graded after a gate fix:** a paired-tag-only regex
  rejected a legal self-closing `w:fldSimple`; the gate was fixed, the run
  re-graded, and the published cell reflects it.
- **The Haiku invocation barrier (a deployment finding, not an engine
  one):** the tool arm's one loss across three sweeps — the
  tracked-footnote lane, 1/3 vs raw-XML's 3/3 — was investigated,
  root-caused, and resolved the same day: the failing runs never invoked a
  stemma tool at all (50 tool-discovery calls, zero engine calls — the
  harness defers tool schemas behind a search step the small model cannot
  reliably cross). Re-run with schemas preloaded: 3/3, and four other
  barrier-hit lanes went to 3/3 with it. Small models need the tool
  schemas loaded upfront.

## Caveats and exclusions

- **n is what it says.** Every cell above carries its replication count;
  most published cells are n=3, wave-1 cells n=5. Directional (n=1) cells
  are labeled and are not claims.
- **Some fixtures are engine-fabricated; Word-verified 2026-07-03.** All
  fixtures and graded wave outputs open clean in real Word, and the
  revision census matches where enumerable (see the
  [clean-open record](benchmarks-history.md#word-clean-open-real-word-all-outputs-all-waves)).
  The residual exposure is the density fixtures' census (oracle timeout)
  and that accept/reject-agreement legs in Word have not been run for
  every fixture family.
- **One lane was excluded as broken:** an insert-a-numbered-clause lane whose
  gate is unsatisfiable on its fixture for *every* arm (the fixture has no
  auto-numbered subsections to join). It needs a re-fixture, not a grade.
- **The resolution lanes' expected outputs were computed by stemma itself at
  freeze time** (accept-all/reject-all projections), which is a
  teaching-to-the-test risk. Mitigations: the fixture family was verified in
  real Word after fabrication (open-clean; accept/reject agreement), and the
  clean-open legs use Word, not stemma. The full neutral-judge pass (every
  arm's output resolved *by Word* and compared) has been run only for the
  hazard suite above, not for every lane.
- **An open-ended "tighten this redline" lane has no automated quality gate**
  (grading requires judgment); it contributes stemma-side economics evidence
  only — its no-stemma arm was withdrawn (see the contamination section) —
  and no quality claim.
- **Fixtures, prompts, and gate scripts are withheld.** Publishing them would
  make the benchmark trivially overfittable (and some fixtures are fabricated
  through held-out real-Word tooling). What is published instead: every lane's
  task in one line, fixture size/shape/provenance class, gate counts, and
  pass-rates. Fixture provenance is deliberately diversified — real-Word
  fabricated, engine-fabricated (Word-verified), and a third-party redline
  over a public financing template — so the suite doesn't only measure
  compatibility with stemma's own quirks.
- **Cost figures are API list-price costs** reported by the CLI per run; they
  exclude retries of failed infrastructure and any human time.

## Reproducibility

| item | value |
|---|---|
| models | waves 1–3 + v2.2 re-runs: `claude-sonnet-4-6` · 2026-07 model sweeps: `claude-sonnet-5` and `claude-haiku-4-5` (per-cell in the [data file](benchmark-data-model-sweeps-2026-07.json)) |
| agent harness | `claude` CLI (Claude Code), headless `claude -p`, `--max-turns 50` |
| CLI versions | 2.1.177 (wave 1, 2026-06-14..15) · 2.1.198 (waves 2–3 + f5 re-grade, 2026-07-02) · the 2026-07 model sweeps ran on the then-current CLI; per-run CLI versions are recorded in the held-out inventory |
| stemma engine | wave 1: pre-release development tree (histories were flattened for release; those exact builds are not bit-recoverable — disclosed, not hidden) · wave 2: the public release at commit `e378b55` · wave 3: the 2026-07-02 development head after same-day fix merges (exact build commit not pinned — disclosed; no cell mixes builds) · v2.2 post-fix re-runs: the release build at commit `b079546` (pinned) · 2026-07 model sweeps: `46206e9` (main lanes) and `880c49a` (negotiation/size lanes) |
| oracle | a real Microsoft Word instance driven as a behavioral oracle (open-clean / accept / reject / resolve) |
| grading | deterministic gate scripts over output `.docx` markup + engine text projections; agent narration never graded |

CLI drift is a real reproduction risk: harness behavior (tool deferral,
context handling) changes between CLI versions and measurably moves economics.
Anyone attempting reproduction should pin the CLI version and expect earlier
waves' economics to shift on newer CLIs.

Every number in this document is generated from a pinned, machine-readable
results inventory (per-run gate outcomes, costs, models, CLI versions,
transcript hashes) maintained with the held-out fixture set; the aggregation
is scripted, not hand-transcribed. Updates to this document follow the same
replication policy stated above.
