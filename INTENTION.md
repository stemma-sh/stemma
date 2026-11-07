# Intention

What Stemma is for, and how to decide when two good things conflict. When a
change, a bug report, or a feature request raises a question this page
answers, follow this page. When it does not, decide, then add the answer here.

## Purpose

Stemma applies requested changes to existing Word documents, faithfully.

A person or an agent says what should change. Stemma makes exactly that change
in the `.docx`, as Word would represent it, and nothing else. By default the
change is a native tracked change, so a human reviews it in Word and accepts
or rejects it. The result opens clean in Word, every revision resolves the way
Word resolves it, and content the request did not touch stays as it was.

The core is the engine: one document model, typed edit operations, and the
laws below. The MCP server, the CLI and the Rust API are surfaces over that
engine. Comparison (`stemma-diff`) is a supported feature, not the core.

## What correct means

In order, each one decides where the ones below it are silent or disagree:

1. **What the requester asked for.** An edit that does not do what was asked,
   or does more, is wrong however well formed it is.
2. **What Word does.** Real Microsoft Word is the referee for how a document
   opens, renders, and resolves. When Word and the specification differ, the
   document must work in Word.
3. **ECMA-376.** The specification decides where Word's behaviour is not
   observed or not observable.
4. **The engine's laws.** One spelling for one state (a saved and reopened
   document is the same model), Accept All and Reject All give the edited and
   the original document, an edit changes only what it targets.

Tests encode these, never the current output.

## Rules for decisions

- **Refuse rather than guess.** An ambiguous request, an unsupported
  construct, or an unknown state is an explicit refusal with a reason the
  caller can act on. A silent best effort is the defect.
- **Never lose content.** Anything the engine does not understand is carried
  through unchanged, or the operation refuses. It is never dropped.
- **Tracked by default.** A change the reviewer cannot see is the exception
  the caller asks for (direct mode), and it must equal accepting the tracked
  change.
- **One spelling.** When two representations mean the same thing, pick one and
  make every path produce it. Do not teach consumers to tolerate both.
- **Fix where the data first goes wrong.** Not where the symptom appears.
- **Selective review must be safe.** Every proposal a reviewer can accept or
  reject on its own must leave a valid document that means what the reviewer
  chose.
- **Measure against reality.** A claim about Word is checked in Word. A claim
  about agents is checked with agents on real documents. Numbers that are
  published are reproducible from the command that produced them.

## Priorities when they compete

1. Correctness of the edited document (the sections above).
2. Agents succeed: a capable agent completes real requests with few calls,
   and a refusal tells it what to do next.
3. Integrators can build on it: a stable, documented API, persistence, and
   review workflows for applications that embed the engine.
4. Comparison quality.
5. Speed and size, once the above hold.

## Non-goals

- Rendering, layout, or page geometry beyond what an edit must preserve.
- Formats other than WordprocessingML (`.docx`, `.dotx`).
- Byte-for-byte preservation for its own sake. The model is the authority;
  bytes are preserved where they carry meaning the model does not.
- A hosted service. Documents are processed where Stemma runs.
