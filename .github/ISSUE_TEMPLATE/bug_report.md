---
name: Bug report
about: A document that imports/edits/exports wrong, a panic, or a wrong result
title: ''
labels: bug
assignees: ''
---

**What happened vs. what you expected**
<!-- The actual behavior, and what the correct behavior would be. -->

**Did this block a deliverable?**
<!-- Yes/no. A refusal is not a completion; say whether the documented task
     could still produce a verified output. -->

**First failing boundary**
<!-- Installation/identity, open/parse, inspect/target, execute/edit,
     verified-delivery/save, or desktop Word adjudication. -->

**Repro**
<!-- The smallest input that triggers it. Attach the `.docx` (or a minimal
     fixture that reproduces), and the exact tool/transaction/API call. If the
     document is sensitive, a stripped-down version that still reproduces is
     ideal. Never attach a real agreement, worklist, or receipt: receipts can
     contain paths, hashes, excerpts, and diagnoses. -->

**Stable codes and content-safe counts**
<!-- Status, deliverable, artifact stage, applied/refused counts, and stable
     refusal/validation codes. Do not paste a receipt, path, document hash, or
     excerpt. -->

**Does Word open the output clean?**
<!-- If this is a serialize/export bug: does the produced `.docx` open in Word
     without a repair dialog? Does Word's accept/reject of the tracked changes
     match what stemma produced? "N/A" if not an output bug. -->

**Surface and identity**
<!-- Which one: stemma apply CLI, engine (Rust api::Document / lower tier),
     stemma-mcp, or stemma-api/editor. Include the source commit or build stamp.
     For an evaluation artifact, include only its public target ID, byte size,
     and SHA-256. -->

**Spec reference (if a conformance claim)**
<!-- If you're asserting what OOXML/Word requires, cite the section:
     ECMA-376 / ISO 29500, or the OPC part (e.g. OPC §9.3). -->
