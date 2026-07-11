---
name: Bug report
about: A document that imports/edits/exports wrong, a panic, or a wrong result
title: ''
labels: bug
assignees: ''
---

**What happened vs. what you expected**
<!-- The actual behavior, and what the correct behavior would be. -->

**Repro**
<!-- The smallest input that triggers it. Attach the `.docx` (or a minimal
     fixture that reproduces), and the exact tool/transaction/API call. If the
     document is sensitive, a stripped-down version that still reproduces is
     ideal. -->

**Does Word open the output clean?**
<!-- If this is a serialize/export bug: does the produced `.docx` open in Word
     without a repair dialog? Does Word's accept/reject of the tracked changes
     match what stemma produced? "N/A" if not an output bug. -->

**Surface**
<!-- Which one: engine (Rust api::Document / lower tier), stemma-mcp,
     stemma-api/editor. Include the commit SHA (pre-1.0: fixes land on main). -->

**Spec reference (if a conformance claim)**
<!-- If you're asserting what OOXML/Word requires, cite the section:
     ECMA-376 / ISO 29500, or the OPC part (e.g. OPC §9.3). -->
