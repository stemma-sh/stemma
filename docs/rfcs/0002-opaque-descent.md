# RFC 0002: opaque descent for textboxes and content controls

- Status: **implemented** (v1 phases 0 through 3b, 2026-07-08; hardened by the
  2026-07-09 implementation review; see Implementation notes)
- Date: 2026-07-07
- Scope: stemma-engine (edit surface, revision census, selective resolution),
  stemma-mcp (verb catalog)

## Problem

The canonical model wraps `w:txbxContent` (textbox interiors), body-level
`w:sdt` (block content controls), and other non-flowing structures as OPAQUE
nodes: verbatim `raw_xml`, preserved but not modeled. That boundary is
deliberate. Fully modeling textbox/SDT interiors, including styles, geometry,
and anchoring, is a fidelity trap. This left a capability gap: **no verb could
edit text inside an opaque region.** An agent asked to fix a typo in a
textbox, or to set a content control's value, got a refusal (fail-loud,
correct, but still a ceiling).

The gap is commercially load-bearing. A census over a held-out corpus of
6,000 wild documents found 75.5% carry some opaque content and **18.1% carry
actionable opaque interiors**; textbox paragraphs with editable text alone
appear in 13.1% of documents, content-control text regions in 5.8%. The most
affected strata are exactly the document classes agents are asked to work on:
policies (27.5% actionable), forms (24.0%), legal (21.2%).

A second, subtler half of the problem: interior tracked changes were
censused (the inventory does not lie) but attributed to the *hosting* story,
so a per-story revision consumer comparing against Word saw a false zero for
the textbox ("text frame") story.

## Design principles

1. **The opaque boundary stays.** Descent is a *scoped edit capability*, not
   a modeling change. Everything outside the edited runs is preserved
   Word-identically (structural re-serialization through the same emitter
   that produced the imported bytes, rather than a byte-for-byte guarantee on
   untouched sibling runs, which may gain normalized form such as an
   explicit `xml:space`).
2. **No silent fallbacks.** A descent edit either applies with full
   tracked-change semantics, or refuses loudly with the reason
   (text not found, region already tracked, span crosses a barrier, region
   not cleanly fillable, partial AlternateContent mirror). No best-effort
   partial writes.
3. **One walk.** Discovery and the revision census share a single
   opaque-interior traversal, so they cannot disagree about which opaques
   carry an interior.
4. **Discovery only advertises what the verbs can edit faithfully.** A
   control whose text hides in a field is not surfaced as fillable; a region
   with pending tracked changes is flagged `has_tracked_changes` (readable,
   not editable until resolved) by the same predicate the verb refuses on.

## Phases (as cited from source as `RFC-0002 §Phase-N`)

- **Phase 0: shared walk and discovery.** `visit_opaque_interiors` is the one
  opaque-interior traversal; `opaque_text_targets` enumerates reachable
  interior text (one target per textbox paragraph / inline-SDT text region,
  with stable addressing and current text); the census routes through the
  same walk.
- **Phase 1: fragment tracked-splice core and `opaque_text_edit`.** An
  XML-native splice inside a parsed opaque fragment: minimal run-level
  `w:ins`/`w:del` (author/date, fresh unique ids) or direct replace.
  `opaque_text_edit` is surgical find→replace inside one addressed textbox
  paragraph or inline content control. "First occurrence" is first in the
  region's document-order visible text, direct runs and transparent wrappers
  (`w:hyperlink`/`w:smartTag`, whose text Word edits freely) interleaved as
  written; the tracked markup lands inside the wrapper. Spans that straddle
  a container boundary, or cross a barrier (tab/break/drawing/field), refuse.
- **Phase 2: `sdt_text_fill` and block-SDT plumbing.** The forms-natural
  whole-value operation ("set this control's value"), shown as a redline or
  applied directly. Inline controls splice their `raw_xml` in place.
  Body-level (block) controls keep their bytes in the serialize scaffold,
  beyond the reach of the pure edit core. The fill is validated and id-minted
  at verb time, staged, applied at save time, and written back into the next
  snapshot's scaffold so discovery reads and subsequent edits see it.
  Block discovery addresses controls by their frozen import-time
  `body_index`.
- **Phase 3: per-story attribution.** `StoryScope::TextFrame` attributes a
  textbox's interior tracked changes to Word's "text frame" story instead of
  the hosting story, closing the per-story false zero.
- **Phase 3b: interior by-id selective resolution.** A well-formed interior
  revision is individually resolvable: selective accept/reject descends into
  the opaque fragment by id and resolves just that carrier, leaving others
  pending and everything else verbatim.

## The resolvability rule (Phase 3b)

The RFC draft proposed "descent-minted revisions resolvable, pre-existing
interior revisions census-only". That distinction does not survive a
serialize/reload cycle. A minted `w:ins` becomes byte-identical to a
pre-existing one, so the distinction is not a model-honest invariant. The
shipped rule is a property of the markup and of the id population:

> An interior revision is individually resolvable ⟺ it is a top-level
> `w:ins`/`w:del` with a non-zero `w:id` that uniquely identifies it
> document-wide.

Each clause is load-bearing:

- **Top-level.** A stacked carrier inside another carrier is not
  individually addressable; the resolver never matches inside a carrier.
- **`w:ins`/`w:del` only.** A move is a pair (`w:moveFrom`+`w:moveTo` plus
  range markers); resolving one half by id would orphan its counterpart.
  Interior moves stay census-only in v1; bulk accept-all/reject-all resolves
  them pair-correctly.
- **Unique document-wide identity.** Import normalizes body/story revision ids
  but deliberately never rewrites opaque bytes, so interior carriers keep raw
  wild wire ids, and wild documents do carry duplicates. An id shared with a
  body revision or another interior carrier identifies nothing: those
  carriers are demoted to census-only (reported honestly, with the reason,
  resolvable via accept/reject-all) rather than letting a selection of one
  silently co-resolve the other. Demotion is re-derived from the current id
  population: resolve the twin away and the id becomes selectable again.

Everything else is censused as `OpaqueInterior` with the never-selectable
sentinel id 0. This includes stacked carriers, `*PrChange`, moves, missing or
zero ids, unparseable carriers, and duplicate ids.

## Explicitly out of scope for v1

Creating/deleting textboxes or SDTs, interior paragraph structure edits
(split/merge), interior formatting edits, field results, OMML, DrawingML
`a:t` shape text (zero hits in the 6,000-document census), nested opaques
beyond one level, and clearing a control to empty. AlternateContent copies
of a textbox are mirrored all-or-refuse by visible-text signature; each copy
mints its own revision ids. Selective resolution of one id resolves one copy,
which is a documented v1 limitation; bulk accept/reject keeps copies consistent.

## Validation

Hermetic validation over the held-out 6,000-document corpus: 715 interior
edits applied and verified reversible (reject-all reconstructs the original
content fingerprint), accept == direct, non-shrinking opaque inventory, all
non-applicable cases refusing loudly, zero content-fidelity violations.
Byte-level comparison of the whole document part is deliberately NOT the
gate. Word-invisible canonicalization (namespace declaration order, rsids,
run re-segmentation) drowns it in false positives; the gate compares a
whitespace-normalized content fingerprint with exact interior text.
By-id interior resolution was validated against real Microsoft Word: our
selective accept/reject of textbox and content-control interior inserts
matches Word's resolution exactly.

A barrier census over 19,485 wild opaque text regions sized the refusal
surface: 77.7% of regions are cleanly editable; ~13.6% refuse where Word
also treats the content as non-text (drawings, field codes, nested
controls). The one measured over-refusal involved text inside hyperlinks and
smart tags (1.8%). Wrapper descent in Phase 1 closed it, bringing targeted
textbox-edit refusals down 85% with zero fidelity violations.

## Implementation notes (2026-07-09 review hardening)

An adversarial implementation review after the v1 merge produced fixes now
part of this RFC's contract:

- duplicate-wild-id demotion under the uniqueness clause above; previously a
  selected body id could silently co-resolve a same-numbered interior
  carrier;
- block-SDT fill write-back to the carried-forward scaffold; previously the
  fill was silently reverted by the next apply and invisible to post-fill
  discovery;
- document-order occurrence selection in the splice (was direct-runs-first);
- minted-id floor over block-opaque interior ids (invisible to the pure
  core's scan);
- `semantic_hash` on a block fill refuses because block discovery surfaces no
  hash, so the guard cannot be honored; duplicate
  block-fill targets in one transaction refuse at the verb edge;
- partial AlternateContent mirrors refuse instead of silently skipping a
  text-matched copy; discovery flags `has_tracked_changes`.

The per-story oracle parity was subsequently re-run against real Microsoft
Word on the held-out interior-revision witness set (2026-07-09, hardened
engine). Every witness passes presence parity per story family. Wherever Word
reports tracked revisions in a story family, our per-story enumeration
now reports them too, including the text-frame story that was the original
blind spot (count deltas between the two inventories are the expected
segmentation difference: our per-carrier records versus Word's Revision
objects, which merge adjacent runs). The by-id interior resolution parity
checks were also re-run on the hardened resolver and still match Word
exactly.

Still open, tracked for a future revision: a curated witness development
suite for interior-heavy documents.
