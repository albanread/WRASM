# Instruction explainer data

`instructions.tsv` is the source for the `instructions` table (loaded by
`cargo run -p winkb --bin import_instructions`), surfaced as the IDE's
per-instruction caret help.

## How it's made (clean-room — see gen/PROCESS.md)

Facts are not copyrightable; prose is. So:

1. The **flags** each instruction modifies are extracted *deterministically*
   from the Intel SDM Vol 2 (`gen/facts.py`) — no model, exact.
2. The **descriptions** are written by the model from its own x86-64 knowledge,
   never reading Intel's wording — original, MIT-clean, and a stronger
   clean-room than paraphrasing.

The Intel PDF and its extracted text are **not** in this repo (Intel's
copyright); only our original prose (`instructions.tsv`) and the deterministic
fact-extraction code (`gen/`). Generation working dir: `E:\documentX8664`.
