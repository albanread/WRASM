#!/usr/bin/env python
"""Deterministically pull the FACTS we need from each extracted Intel page.

No LLM, no tokens, no prompts. Output facts.json: one record per instruction with
the mnemonic + aliases, the title, a sample of operand forms, and the flags the
instruction MODIFIES (parsed from the "Flags Affected" section). The flags become
the oracle that check.py compares the model's prose against.
"""
import json
import os
import re

EXTRACTED = "extracted"
FLAGS = ["CF", "PF", "AF", "ZF", "SF", "OF", "DF", "IF", "TF", "NT", "AC", "RF"]
DASH = re.compile(r"\s*[‐-―−�-]\s*")
# Section headers that end the "Flags Affected" block.
NEXT_SEC = re.compile(
    r"\n\s*(Operation|Protected Mode Exceptions|Real-Address Mode Exceptions|"
    r"Virtual-8086 Mode Exceptions|Compatibility Mode Exceptions|64-Bit Mode Exceptions|"
    r"Exceptions|SIMD Floating-Point Exceptions|FPU Flags Affected|Intel C/C\+\+)"
)


def header(text):
    # The real running header `MNEM—Title` is a short dash line in the body (we
    # prepended the bare mnemonic as line 1, so skip to the first dash header).
    lines = [l.strip() for l in text.splitlines() if l.strip()]
    line = next((l for l in lines if DASH.search(l) and len(l) < 80), lines[0] if lines else "")
    parts = DASH.split(line, 1)
    left = parts[0].strip()
    title = (parts[1].strip() if len(parts) > 1 else "").split("  ")[0].strip()
    title = re.sub(r"\s+Vol\..*$", "", title).strip()
    bits = [b.strip().lower() for b in left.split("/") if b.strip()]
    mnem = bits[0] if bits else left.lower()
    return mnem, " ".join(bits[1:]), title


def flags_modified(text):
    i = text.find("Flags Affected")
    if i < 0:
        return [], ""
    sec = text[i + len("Flags Affected"):]
    m = NEXT_SEC.search(sec)
    sec = sec[: m.start()] if m else sec[:700]
    sec = sec.strip()
    if sec[:5].lower().startswith("none"):
        return [], sec
    # Classify per sentence: a flag is "modified" only if ITS sentence doesn't say
    # undefined/unaffected (so 'CF is not affected. OF/SF/... are set' is exact).
    mod = set()
    for sent in re.split(r"(?<=[.;])\s+", sec):
        sl = sent.lower()
        unaff = any(w in sl for w in ("undefined", "unaffected", "not affected", "unchanged", "not modified"))
        if unaff:
            continue
        for fl in FLAGS:
            if re.search(r"\b" + fl + r"\b", sent):
                mod.add(fl)
    return [f for f in FLAGS if f in mod], sec


def forms(text, mnem):
    out, seen = [], set()
    pat = re.compile(r"\b" + re.escape(mnem.upper()) + r"\s+[A-Za-z][\w/, *]*")
    for m in pat.finditer(text):
        s = re.sub(r"\s+", " ", m.group(0)).strip()
        if 4 < len(s) < 40 and s not in seen and "," in s:
            seen.add(s)
            out.append(s)
        if len(out) >= 6:
            break
    return out


def main():
    recs = []
    for fn in sorted(os.listdir(EXTRACTED)):
        if not fn.endswith(".txt"):
            continue
        text = open(os.path.join(EXTRACTED, fn), encoding="utf-8").read()
        mnem, aliases, title = header(text)
        mod, raw = flags_modified(text)
        recs.append({
            "file": fn[:-4], "mnemonic": mnem, "aliases": aliases, "title": title,
            "forms": forms(text, mnem), "flags_modified": mod,
            "flags_raw": re.sub(r"\s+", " ", raw)[:300],
        })
    json.dump(recs, open("facts.json", "w"), indent=0)
    none = sum(1 for r in recs if not r["flags_modified"])
    print(f"facts for {len(recs)} instructions -> facts.json  ({none} modify no flags)")
    for r in recs[:4] + [x for x in recs if x["mnemonic"] in ("add", "shl", "lea")]:
        print(f"  {r['mnemonic']:8} flags={r['flags_modified']}  forms={r['forms'][:2]}  | {r['title'][:40]}")


if __name__ == "__main__":
    main()
