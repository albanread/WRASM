#!/usr/bin/env python
"""Split the Intel SDM Vol 2 PDF into one text file per instruction.

Each instruction reference page carries a running header `MNEMONIC—Title`;
consecutive pages with the same header belong to one instruction. We group them
and keep the ones that actually look like an instruction reference (they have a
Description and an Opcode/Flags section). Output: extracted/<MNEMONIC>.txt.
"""
import os
import re

import pypdf

SRC = "intel-sdm-vol2.pdf"
OUT = "extracted"
# MNEMONIC then a dash-like char then a name. Allow lowercase tails (Jcc, SETcc).
DASH = "‒-―‐‑−�\\-"
HDR = re.compile(rf"^\s*([A-Z][A-Za-z0-9/ ]{{0,28}}?)\s*[{DASH}]\s*\S")


def first_line(t):
    for ln in t.splitlines():
        if ln.strip():
            return ln
    return ""


def main():
    os.makedirs(OUT, exist_ok=True)
    r = pypdf.PdfReader(SRC)
    groups = []  # (mnemonic, [page_text, ...])
    cur, pages = None, []
    for p in r.pages:
        t = p.extract_text() or ""
        m = HDR.match(first_line(t))
        mn = m.group(1).strip() if m else None
        if mn and mn == cur:
            pages.append(t)
        elif mn:
            if cur:
                groups.append((cur, pages))
            cur, pages = mn, [t]
        elif cur:
            pages.append(t)  # continuation page without a clean header
    if cur:
        groups.append((cur, pages))

    kept = 0
    for mn, ps in groups:
        blob = "\n".join(ps)
        if "Description" in blob and ("Opcode" in blob or "Flags Affected" in blob):
            fn = re.sub(r"[^A-Za-z0-9]+", "_", mn).strip("_")
            if fn:
                with open(os.path.join(OUT, f"{fn}.txt"), "w", encoding="utf-8") as f:
                    f.write(f"{mn}\n\n{blob}")
                kept += 1
    print(f"groups: {len(groups)}  kept: {kept}")


if __name__ == "__main__":
    main()
