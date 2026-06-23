# Efficient clean-room instruction explainer — process v2

## What v1 got wrong

v1 spawned **~1500 agents** (752 instructions × a *writer* + a *verifier*), and
**every agent read a full Intel page** (1–5 KB). That is the root of both
complaints:

- **(a) 1500 approval prompts** — each agent's `Read` of a file outside the
  project asks for permission.
- **(b) ~44M tokens** — a long page in, reasoning, prose out — *doubled* by the
  verifier re-reading the same page.

## The fix: facts are cheap, prose is the model's own

Only one thing must come from Intel: the **facts** (which flags an instruction
modifies, its operand forms). Those extract **deterministically with Python** —
no LLM, no tokens, no prompts. The **prose** is written by the model from its
own (extensive, accurate) x86 knowledge, *grounded by the extracted facts passed
inline* — so the writer **never reads Intel's wording** (a stronger clean-room)
and **uses no tools** (so it never triggers a permission prompt).

```
1. facts.py     (Python, free)   extracted/<M>.txt → facts.json
                                 {mnemonic, aliases, title, forms[], flags_modified[]}
2. write        (LLM, batched)   ~40 instructions per agent, facts INLINE, NO file reads,
                                 original house-style prose from knowledge → entries
3. check.py     (Python, free)   compare each entry's flags to facts.json → discrepancies
4. fix          (LLM, tiny)      review ONLY the flagged discrepancies
5. aggregate.py (Python, free) → instructions.tsv → import_instructions → DB
```

## The numbers

| | v1 (brute force) | v2 (this) |
|---|---|---|
| agents | ~1500 | ~20 |
| approval prompts | ~1500 (file reads) | **~0** — writers use no tools |
| tokens | ~44M | **~0.3M** (~150× less) |
| clean-room | read Intel, re-express | **never reads Intel prose** (stronger) |
| correctness | a second LLM re-reads | **deterministic flag check** vs the real Intel page |

## Why it's still correct and clean-room

- **Flags are grounded in the actual Intel page** — `facts.py` parses the
  "Flags Affected" section, the single fact most worth pinning down, and the
  check compares every entry against it exactly.
- **The prose is the model's own wording** — it never saw Intel's sentences, so
  it cannot copy them. Genuinely clean-room.
- The deterministic flag check catches the common mistake; only the handful of
  discrepancies (and any instruction the writer marks low-confidence) get a
  small LLM review pass — not 752 of them.

## Batching

`write` runs as one small workflow: the instruction list is split into chunks of
~40, each chunk handed to one agent **with that chunk's facts inline**. ~19
agents cover all 752. No agent reads a file; each returns ~40 structured entries.
