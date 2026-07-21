# Benchmark archive: per-tier tables and wave history

This is the archival record behind [the benchmark report](benchmarks.md):
per-lane tables for the older/cheaper model pins, the document-size detail,
and the full wave-by-wave history, including the cells that were later
corrected, disqualified, or overturned, kept here so the corrections stay
auditable. The headline numbers, methodology, caveats, and every claim we
currently stand behind live in the main report; nothing here supersedes it.
Every number is machine-checkable against the
[per-cell data](benchmark-data-model-sweeps-2026-07.json) or, for the wave
tables, the held-out results inventory described in the main report's
Reproducibility section.

## Per-lane tables by model pin

Identical columns to the main table so tiers compare like-for-like; see the
main report for column definitions.

### Sonnet 4.6

Historical replicated runs used June-era engine builds, so this table compares
both model and engine era. Each cell has 3 to 5 runs. Three vanilla cells never ran on
this pin.

| task | stemma success | vanilla success | stemma latency | vanilla latency | stemma tokens ÷ vanilla |
|---|---|---|---|---|---|
| stacked-revision resolution | 3/3 | 2/3 | 76s | 188s | 0.75× |
| cascading resolution | 3/3 | 3/3 | 60s | 59s | 0.70× |
| paragraph-join resolution (§17.13.5.15) | 3/3 | 3/3 | 30s | 190s | 0.36× |
| table-interior revision resolution | 5/5 | 3/5 | 30s | 251s | 0.26× |
| paragraph-formatting revision (pPrChange) | 3/3 | 3/3 | 37s | 74s | 0.63× |
| table/cell formatting-change selective resolution | 3/3 | 3/3 | 38s | 121s | 0.40× |
| tracked table-row add + delete | 3/3 | not run | 57s | not run | not run |
| tracked whole-section delete, renumbering | 3/3 | 3/3 | 28s | 40s | 1.14× |
| tracked bold on defined terms (rPrChange) | 3/3 | 2/3 | 100s | 81s | 0.83× |
| flatten a redline to a clean final | 3/3 | 0/3 | 27s | 54s | 0.56× |
| compare base vs revised into a redline | 3/3 | 3/3 | 10s | 216s | 0.14× |
| NDA end-to-end edit | 4/5 | 1/5 | 336s | 358s | 0.46× |
| selective accept/reject by author | 3/3 | 3/3 | 68s | 250s | 0.49× |
| policy-manual multi-edit | 3/5 | 5/5 | 406s | 407s | 0.60× |
| product-spec edit | 5/5 | 5/5 | 280s | 188s | 0.66× |
| tracked clause authoring | 3/3 | 3/3 | 31s | 32s | 1.15× |
| edit inside an existing redline | 5/5 | 5/5 | 107s | 64s | 2.21× |
| refusal / no-corruption under ambiguity | 4/5 | 4/5 | 209s | 261s | 0.31× |
| add a comment | 3/3 | 3/3 | 29s | 30s | 0.87× |
| insert an image with caption | 3/3 | 3/3 | 397s | 80s | 2.11× |
| insert a native ToC field | 3/3 | not run | 25s | not run | not run |
| tracked edit inside a footnote body | 3/3 | not run | 29s | not run | not run |
| selective resolution @ ~300 markers | 3/3 | 3/3 | 121s | 305s | 0.30× |
| @ ~1,000 markers | 3/3 | 3/3 | 149s | 345s | 0.78× |
| @ ~3,050 markers | 3/3 | 3/3 | 224s | 427s | 0.50× |

### Haiku 4.5

Pinned wave 2026-07-04, same engines as the Sonnet 5 wave, n=3 per cell.
Deferred tool-schema loading depressed several stemma cells. See the
invocation-barrier finding under the main report's
[corrections](benchmarks.md#corrections-after-publication).

| task | stemma success | vanilla success | stemma latency | vanilla latency | stemma tokens ÷ vanilla |
|---|---|---|---|---|---|
| stacked-revision resolution | 3/3 | 0/3 | 238s | 35s | 1.08× |
| cascading resolution | 2/3 (1 DNF) | 0/3 | 25s | 68s | 0.35× |
| paragraph-join resolution (§17.13.5.15) | 3/3 | 1/3 | 18s | 37s | 0.64× |
| table-interior revision resolution | 3/3 | 0/3 | 23s | 46s | 0.87× |
| paragraph-formatting revision (pPrChange) | 3/3 | 3/3 | 88s | 32s | 1.03× |
| table/cell formatting-change selective resolution | 2/3 | 1/3 | 19s | 60s | 0.36× |
| tracked table-row add + delete | 3/3 | 0/3 | 86s | 36s | 1.49× |
| tracked whole-section delete, renumbering | 3/3 | 0/3 | 30s | 33s | 1.06× |
| tracked bold on defined terms (rPrChange) | 3/3 | 0/3 | 32s | 38s | 0.79× |
| flatten a redline to a clean final | 2/3 | 0/3 | 111s | 30s | 0.83× |
| compare base vs revised into a redline | 3/3 | 1/3 | 27s | 154s | 0.12× |
| NDA end-to-end edit | 0/3 | 0/3 | 64s | 84s | 0.78× |
| selective accept/reject by author | 3/3 | 0/3 | 23s | 129s | 0.21× |
| policy-manual multi-edit | 0/3 (1 DNF) | 1/3 | 165s | 112s | 1.10× |
| product-spec edit | 3/3 | 0/3 | 96s | 86s | 1.01× |
| tracked clause authoring | 3/3 | 3/3 | 20s | 30s | 0.61× |
| edit inside an existing redline | 3/3 | 0/3 | 54s | 38s | 1.25× |
| refusal / no-corruption under ambiguity | 2/3 | 0/3 | 96s | 125s | 0.97× |
| add a comment | 3/3 | 1/3 | 19s | 44s | 0.39× |
| insert an image with caption | 0/3 | 0/3 | 352s | 56s | 1.56× |
| insert a native ToC field | 3/3 | 2/3 | 16s | 35s | 0.66× |
| tracked edit inside a footnote body | 1/3 (2 DNF) | 3/3 | 193s | 29s | 15.31× |
| selective resolution @ ~300 markers | 2/3 | 1/3 | 59s | 89s | 0.38× |
| @ ~1,000 markers | 2/3 | 0/3 | 103s | 61s | 0.75× |
| @ ~3,050 markers | 2/3 | 1/3 | 312s | 128s | 0.87× |
| negotiation loop: selective resolve + tracked counter + comment | 0/3 (1 DNF) | 0/3 | 109s | 63s | 1.73× |
| nested revisions: accept A, reject B-inside-A | 2/3 | 0/3 | 114s | 51s | 0.83× |
| negotiation loop in a ~50-page agreement | 0/3 | 0/3 | 115s | 110s | 1.18× |
| negotiation loop in a ~150-page agreement | 0/3 | 0/3 | 155s | 163s | 1.54× |

Reading the Haiku table:

- Hand-editing OOXML collapses without a frontier model: Haiku's raw-XML arm
  fails even the lanes it aced on Sonnet 5 (f3, f5, auth-6, tracked table
  rows). Behind the engine, Haiku stays at or near 3/3 on the entire
  resolution family, including the lanes Sonnet 5's raw-XML arm fails,
  at roughly $0.10 to $0.25 per document.
- The interface does not make a small model a large one: Haiku went 0/3 on
  every compound multi-step task (do-these-five-things-in-one-pass), while
  staying near-perfect on single-intent operations. Cheap models are an
  operational tier for decomposed calls, not planners.
- Faster tokens did not mean faster tasks: on lanes Haiku handles cleanly it
  is 1.3 to 4× faster than Sonnet 5, but retry loops on the lanes it fumbles
  make its overall median *slower*. Task latency is turn count, not token
  rate.
- The tracked-footnote cell scored 1/3 versus raw-XML's 3/3, making it the
  tool arm's one loss in three sweeps. It was root-caused the same day as the invocation barrier
  (the failing runs never invoked a stemma tool at all) and resolved by
  preloading tool schemas; see the main report's
  [corrections](benchmarks.md#corrections-after-publication).
- Per-run forensics on every stemma-arm failure (both sweeps) are part of
  the suite's method; one previously-reported failure in this table was
  reclassified after forensics as a grading bug (the gate rejected a legal
  self-closing `w:fldSimple`). The gate was fixed, the run re-graded, and
  the number above reflects it.

## Document size: flat vs. scaling (2026-07-04)

The following table gives the full detail behind the two negotiation-loop
size rows in the main table. Both use the identical task (the same 43 tracked
changes by two authors, the same prompt, and targets placed at least 75% deep)
in a ~50-page and a ~150-page agreement, so document size is the only
variable. Sonnet 5, n=3:

| document | arm | success | turns | latency | tokens |
|---|---|---|---|---|---|
| ~50 pages | stemma | 3/3 | 28 | 133s | 1.77M |
| ~50 pages | vanilla | 3/3 | 37 | 205s | 2.62M |
| ~150 pages | stemma | 2/3 | 29 | 144s | **1.57M** |
| ~150 pages | vanilla | 3/3 | 46 | 261s | **3.47M** |

Tripling the document moved stemma by one turn, and its token traffic did
not grow at all: the document stays server-side and the agent's traffic
(outline, targeted reads, receipts) is size-invariant. The vanilla arm must
move the XML through the context window and scales with it: +33% tokens
and +9 turns for the same 43 changes, its median run at 46 of 50 allowed
turns, one rung from the ceiling. (stemma's one dropped rep at 150pp was an
engine refusal fixed the same week; see the repo history for
`comment_create` tracked-anchor support; the ladder predates the fix.) The
engine itself resolves the 150-page redline in under two seconds. Every
second in the table is agent loop, not engine.

## Wave history

Three replication waves, all on the `claude-sonnet-4-6` pin. Wave 1
(2026-06): the seven decision-critical lanes at n=5 per arm. Wave 2
(2026-07-02 morning): the remaining gated lanes at n=3, on the
public-release engine. Wave 3 (2026-07-02 afternoon, suite v2.1): a
revision-density ladder plus eight fresh capability probes at n=3, on the
same-day development head. No cell mixes engine builds. "Pass" = every
deterministic gate passed.

### Wave 1: decision-critical lanes (n=5 per arm)

| lane | task | fixture | stemma | vanilla | verdict |
|---|---|---|---|---|---|
| res7 | produce accept-all + reject-all copies of a redline (roundtrip fidelity) | 6 KB, 47 revision markers, 2 authors | **5/5** · $0.30 | 2/4\* · $0.85 | **stemma win**, the one replicated quality moat |
| f4 | move a section as a tracked change; auto-numbering + cross-refs must survive | 18 KB, ~10 pp, 0 revisions | 3/5 · $0.95 | **5/5** · $1.32 | **inversion: vanilla wins** |
| f1 | apply counsel's list of 8 edits as tracked changes on a clean NDA | 18 KB, 6 to 8 pp, 0 revisions | 4/5 · $0.87 | 1/5 · $1.30 | stemma edge, both arms flippy |
| res8 | selective formatting-change resolution (v1 prompt) | 6 KB, 47 markers | 2/5 · $0.28 | **5/5**† · $0.40 | **inversion: vanilla wins** under the v1 prompt (see the main report's corrections) |
| f5 | edits adjacent to opaque objects (figures, footnote-in-sentence, hyperlink, term replace) | 43 KB, ~8 pp, images/equation/fields | 5/5 · $0.74 | 5/5 · $0.84 | **tie** |
| auth6 | layer a tracked edit inside another author's pending insertion | 129 KB, 367 markers | 5/5 · $0.65 | 5/5 · **$0.34** | stable tie; **vanilla cheapest** |
| safe6 | tighten a redline without an author identity while never impersonating the existing reviewer | 129 KB, 367 markers | 4/5 · $0.69 | 3/4\* · n/p | tie (both flippy); no clean run ever impersonated |

† corrected 2026-07-02: originally published as vanilla 0/5. The gate
string-compared serialized XML instead of the properties it encodes. This had
failed every vanilla run on a whitespace-only serializer-dialect diff while
passing stemma's own (same-serializer) output. Gate fixed, all runs re-graded;
see the main report's
["Corrections after publication"](benchmarks.md#corrections-after-publication).
stemma's 2/5 stands: its misses left the change pending where the v1 prompt
said "keep" (a defensible reading of an ambiguous word; the v2 lane below
settles it).

\* one vanilla run per starred cell was **disqualified by transcript audit**
(see the main report's
[contamination section](benchmarks.md#benchmark-awareness-contamination)):
the "vanilla" agent located the stemma engine on the host and drove it to
produce its output. Disqualified runs are excluded from the cell (they don't
measure the arm they're labeled as), which is why those cells read /4.
"n/p" = cost not published (see the same section).

Notes, in honesty order:

- **f4 inverts.** Over 5 runs each, the raw-XML agent is *more* reliable than
  the stemma agent at tracked structural moves (5/5 vs 3/5), and both open
  clean in Word. A single earlier run had suggested the opposite; replication
  killed that reading.
- **f5 is a tie, not a win.** An earlier single-run matrix scored it
  19-gates-vs-18 for stemma; re-graded against the committed frozen ground
  truth, every stemma *and* vanilla run on record passes all 19 gates (5/5
  each).
- **safe6's "impersonation moat" dissolved.** Across the 9 clean tighten runs,
  neither arm ever authored edits as the existing reviewer. stemma's one miss
  was under-scoping (it added no authored edits that run), not impersonation;
  vanilla's one miss was a suite-invariant violation (it edited the input file
  in place), not impersonation either.
  stemma's write surface does refuse author impersonation *by construction*,
  but this benchmark never caught a competitor committing that failure, so it
  is reported as an engine property, not a demonstrated competitive failure.
  The vanilla cost for this lane is not published: the underlying task's
  frozen prompt names the stemma plugin, which sends a no-stemma agent
  hunting for it. The authorship *gates* over the hand-made outputs are
  unaffected, but the *economics* of those runs don't measure raw-XML editing.
- **auth6 is the economics counterexample:** vanilla is ~2× cheaper there
  ($0.31 to $0.39 versus $0.61 to $0.88, with no spread overlap).
- **res8 inverts once its gate is honest.** Vanilla hand-materialized the
  formatting-change acceptance correctly in all 5 runs; stemma's 2/5 comes
  from reading "keep this change" as leave-pending. With the prompt
  disambiguated (res8v2 in the wave-2 table), both arms pass 3/3. The lane
  is a tie, and the earlier "vanilla can't resolve formatting changes"
  reading is withdrawn.

### Wave 2: remaining gated lanes (n=3 per arm, public-release engine)

Run 2026-07-02 on the first public release build under CLI 2.1.198,
after the harness hardening described in the main report's contamination
section; transcript audit clean on all 36 runs. All lanes below share either
the 6 KB / 47-marker engine-fabricated redline, the 18 KB real-Word NDA, or
(cmp1) a base + revised pair of the NDA.

| lane | task | stemma | vanilla | verdict |
|---|---|---|---|---|
| res4 | mixed resolution of a cross-author stacked pair (accept insertion, reject nested deletion) | **3/3** · $0.29 | 2/3 · $0.59 | stemma edge; vanilla's miss rejected far more than the asked scope |
| res5 | reject an insertion containing a nested deletion (cascade) | 3/3 · $0.21 | 3/3 · $0.26 | tie, cost near parity |
| res6 | accept a tracked paragraph-mark deletion (paragraph join) | 3/3 · $0.18 | 3/3 · $0.64 | tie; stemma ~3.5× cheaper |
| auth2 | change exactly one of 11 occurrences of a phrase | 3/3 · $0.16 | 3/3 · $0.19 | tie, cost near parity |
| f3 | selective triage: accept author A everywhere, reject author B in one section | 3/3 · $0.26 | 3/3 · $0.99 | tie; stemma ~4× cheaper |
| cmp1 | produce a redline from base + revised (accept-all ≡ revised, reject-all ≡ base) | 3/3 · $0.11 | 3/3 · $0.85 | tie; stemma ~8× cheaper |
| res8v2 | selective formatting-change resolution, prompt disambiguated ("accept" instead of "keep"; disclosed suite bump) | 3/3 · $0.22 | 3/3 · $0.33 | tie; stemma modestly cheaper |

Wave-2 honesty notes:

- **Quality is a near-uniform tie.** A capable raw-XML agent hand-resolves
  stacked pairs, cascades, and paragraph joins correctly at n=3. The one
  stemma quality edge (res4) came from a single vanilla run resolving far
  more than the requested scope.
- **The economics gap is task-shaped, not universal.** Under the newer CLI,
  vanilla reached near cost-parity on the surgical single-edit lanes (auth2,
  res5, res4), the earlier "stemma is always 2 to 5× cheaper" reading does not
  hold there. The gap stays large (roughly 4 to 8×) where the task forces bulk output
  through the context window: full triage (f3), producing whole documents
  (cmp1, res6's clean copy). Same confound caveat as wave 1, in both
  directions.
- Wave-2 cells are not comparable 1:1 with the June directional singles of
  the same lanes (different engine build and CLI); the superseded singles
  agree directionally and remain in the held-out run inventory.

### Wave 3: density ladder and capability probes (n=3 per arm, suite v2.1)

Run 2026-07-02 (afternoon) under CLI 2.1.198; transcript audit clean on all
66 runs. Two questions: does the resolution tie hold as revision density
scales by 10×, and what happens on document verbs *outside* the resolution
family (tables, comments, images, footnotes, formatting, ToC, flatten)?
All fixtures are fresh, engine-fabricated for this wave; at first
publication their real-Word verification was pending (the oracle was
unreachable). The sweep ran 2026-07-03 and is folded into the clean-open
section below: all fixtures and outputs open clean, and Word's own
revision census matches the frozen marker counts where Word can enumerate
them. The engine build was the 2026-07-02 development head with same-day
fix merges. The exact build commit was not pinned, which is disclosed here;
each lane's replications ran back-to-back on one binary). Runs that hit the
turn ceiling with no output count as **failures**, not exclusions.

**Density ladder:** the same two-author selective-resolution task on a
~61 pp agreement, fabricated at three revision densities:

| lane | markers | stemma | vanilla | verdict |
|---|---|---|---|---|
| scale-d1 | 304 | 3/3 · $0.58 ($0.44 to $0.68) | 3/3 · $1.04 ($0.52 to $2.01) | tie on quality |
| scale-d2 | 1,016 | 3/3 · $0.69 ($0.67 to $0.72) | 3/3 · $1.00 ($0.82 to $1.72) | tie on quality |
| scale-d3 | 3,050 | 3/3 · $0.80 ($0.63 to $0.87) | 3/3 · $1.20 ($1.08 to $1.26) | tie on quality |

There is **no density cliff for either arm**. The mechanism is worth
stating: at these densities the vanilla agent stops hand-editing and writes
itself a small resolver program over the XML, so marker count barely moves
its cost because it effectively rebuilds a document engine per task. stemma stays
roughly 1.5 to 2× cheaper with a much tighter spread (from $0.58 to $0.80 across a 10×
density increase, versus vanilla's $0.52 to $2.01 run-to-run swing at the lowest
tier). The economics confound from earlier waves applies here too.

**Capability probes:** one verb per lane, using small single-purpose fixtures:

| lane | task | stemma | vanilla | verdict |
|---|---|---|---|---|
| auth-d1 | tracked whole-section delete; native renumbering must survive | 3/3 · $0.21 | 3/3 · $0.20 | tie |
| cmt-1 | add a comment (annotation; revision census unchanged) | 3/3 · $0.21 | 3/3 · $0.20 | tie |
| img-1 | insert a provided image with caption | 3/3 · $0.95 | 3/3 · $0.33 | tie; **vanilla ~3× cheaper** |
| fmt-a1 | tracked bold on defined terms (a true formatting change, not text churn) | **3/3** · $0.35 | 2/3 · $0.31 | stemma edge |
| cmp-f1 | flatten a two-author redline (incl. footnote-story revisions) to a clean final copy | **3/3** · $0.16 | **0/3** · $0.25 | **stemma win because vanilla silently emptied the footnotes in all 3 runs** |
| tbl-s1 | add + delete tracked table rows | 1/3 · $0.66 | **3/3** · $0.26 | **inversion: vanilla wins** |
| toc-1 | insert a native, Word-updatable ToC field | 0/3 · $0.32 | 0/3 · $0.25 | **both arms fail** |
| fn-a1 | tracked correction inside a footnote body | **0/3** (2 hit the turn ceiling) · $1.57 ($1.35 to $19.37) | **3/3** · $0.18 | **inversion: vanilla wins** |

Wave-3 honesty notes, worst-for-stemma first:

- **fn-a1 is stemma's worst cell in the entire suite.** The tool surface had
  no tracked path into footnote bodies. Worse, its note-editing verb
  neither supported tracked mode nor *refused* when the task demands it. One
  run edited the footnote untracked (caught by the no-untracked-mutations
  gate); the other two recognized untracked output would be wrong and burned
  the full 50-turn budget hunting for a tracked path that does not exist.
  A missing refusal costs as much as the missing capability: the graded run
  spent $19.37. The raw-XML agent just did it, 3/3 at $0.18.
- **tbl-s1 is an interface loss, not an engine loss.** An engine-level probe
  round-trips tracked row insert+delete correctly; the failing agent runs
  flailed between the addressing schemes of the row-op family and ended up
  shipping untracked row surgery. Same genus as the wave-1 f4 loss:
  ergonomics of the write surface, measured honestly as a loss.
- **toc-1 is a symmetric zero with asymmetric causes.** stemma simply had no
  ToC verb, so no run could produce a native field. Vanilla inserted a
  plausible ToC field every time but *also* mutated body text it had no
  reason to touch: the same typographic-quote substitution in all three
  runs, caught by the content-otherwise-unchanged gate.
- **cmp-f1 is the strongest stemma-favorable replication of the wave.** All
  three vanilla runs produced a clean-looking final document whose footnote
  text was *empty*. This silent content loss was reported as success by the agent
  every time. This is exactly the failure class the suite's MUST-NOT gates
  exist for, and the first time a competitor arm committed it reproducibly.
  Disclosure: the engine capability this lane exercises (story-aware
  revision handling) merged the same day, motivated by an engine gap found
  while fabricating held-out fixtures for the res8 correction. The fix
  predates this lane's runs and was not derived from them.
- **fmt-a1's vanilla miss is a fidelity trap worth naming:** it materialized
  the formatting change as delete + re-insert of the text, which reads as
  text churn to every downstream reviewer instead of a formatting-only
  change. Two of three vanilla runs did author it correctly.
- The ties (auth-d1, cmt-1, img-1) are reported with the same weight as the
  losses: comments, image insertion, and section deletion give stemma no
  quality edge at n=3, and img-1's vanilla runs were ~3× cheaper.

### Post-fix re-runs (suite v2.2, 2026-07-03, disclosed suite bump)

The three wave-3 losses were fixed the next day (a native ToC block on the
insert verb; tracked authoring into footnote/endnote bodies with loud
refusals for anything untrackable; atomic row-insert-with-content plus an
honest own-insert/foreign-change distinction on cell edits) and the stemma
arm was re-run cold, n=3 per lane, on a pinned post-fix engine build. The
fixes were designed from the failing transcripts' *interface shapes* (what
payloads agents guessed, where they dead-ended), never from fixture
content or gate internals; prompts, fixtures, and gates are unchanged from
wave 3. The vanilla cells stand (nothing changed on that side); the wave-3
stemma cells above remain in the record as the pre-fix baseline:

| lane | stemma (pre-fix, wave 3) | stemma (post-fix, v2.2) | vanilla (wave 3) |
|---|---|---|---|
| toc-1 | 0/3 | **3/3** · $0.20 | 0/3 |
| fn-a1 | 0/3 (2 DNF, worst run $19.37) | **3/3** · $0.22, 6 to 7 turns | 3/3 · $0.18 |
| tbl-s1 | 1/3 | **3/3** · $0.29 | 3/3 · $0.26 |

Read honestly: fn-a1 and tbl-s1 became ties because vanilla was already
passing. The fix removes stemma's dead ends and the $19-DNF failure mode rather
than beating anyone), and toc-1 became a stemma-only capability on this
suite (vanilla's 0/3 body-text mutation stands). Transcript audit clean on
all 9 runs. The 2026-07-03 oracle sweep additionally confirmed all nine
outputs open clean in real Word AND that Word classifies the new verbs'
machinery as real revisions (the footnote correction as insert+delete in
the footnotes story; the row surgery as cell insertions/deletions; the ToC
as a tracked insert), rather than silently ignored markup. Remaining caveat:
same-fixture re-runs after a fix are the weakest form of validation this
suite accepts. Fresh held-out fixtures for these three capability classes
are the planned stronger check. (The 2026-07 model sweeps in the main
report subsequently re-measured all three capabilities on fresh waves.)

## Word clean-open (real Word, all outputs, all waves)

Every wave-1 output docx was opened by a real Microsoft Word instance:
**stemma 35/35 clean, vanilla 35/35 clean.** An earlier single run in which
a vanilla output tripped Word's repair dialog did **not** replicate.
Clean-open does not discriminate between a careful raw-XML agent and stemma
on these lanes. That earlier "headline" was noise, and we retract it
explicitly.

The 2026-07-03 sweep extended this to everything published since: **every
graded output of waves 2 and 3 and the v2.2 post-fix re-runs opens clean:
124/124, zero repairs, across both arms.** The eleven wave-3 fixtures also
open clean, and Word's own revision inventory matches the frozen census
where Word can enumerate it (the flatten fixture's 8 markers exactly; the
zero-marker capability fixtures at zero). The three density-ladder
fixtures open clean but their census (304 to 3,050 markers) exceeds the
oracle's per-call Word timeout, so those three counts are engine-frozen
only, which is disclosed here.
