//! Two-pass assembler driver: assembled text → [`EncodedModule`].
//!
//! 1. Parse every line into an [`Item`] (code bytes / data / label / globl /
//!    align / relaxable branch).
//! 2. Branch relaxation: internal `jmp`/`jcc` start short (rel8) and grow to
//!    rel32 only when the displacement overflows i8 — iterated to a fixpoint
//!    (branches only grow, so it converges). This mirrors LLVM-MC's
//!    start-short/relax-on-overflow policy, for byte-identity. `call` is always
//!    rel32; branches to externs are always rel32.
//! 3. Emit: lay out final bytes, patch internal branch + RIP-rel displacements,
//!    and emit a [`Reloc`] for every reference to a symbol not defined here
//!    (host externs).

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use crate::backend::{EncodedModule, Reloc, RelocKind};

use super::encode::{encode, FixupKind};
use super::parse::{parse_line, Directive, Line, Operand};

/// Which output section an item lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sect {
    /// `.text` — read-execute code.
    Text,
    /// `.data` — read-write globals.
    Data,
}

/// A relaxable branch/call.
struct Branch {
    /// Short-form opcode bytes (1-byte rel8 follows). `None` for `call`.
    short: Option<Vec<u8>>,
    /// Long-form opcode bytes (4-byte rel32 follows).
    long: Vec<u8>,
    target: String,
    /// Decided during relaxation.
    is_long: bool,
}

impl Branch {
    fn size(&self) -> usize {
        if self.is_long {
            self.long.len() + 4
        } else {
            self.short.as_ref().unwrap().len() + 1
        }
    }
}

enum Item {
    /// Fixed code/data bytes, with RIP-rel fixups (offset-within-bytes, target).
    Code { bytes: Vec<u8>, riprel: Vec<(usize, usize, String, i32)> },
    Label(String),
    Globl(String),
    /// `.align`/`.p2align` — pad to a 2^n boundary (n already normalized).
    AlignP2(u32),
    Branch(Branch),
}

impl Item {
    fn size_at(&self, off: usize) -> usize {
        match self {
            Item::Code { bytes, .. } => bytes.len(),
            Item::Label(_) | Item::Globl(_) => 0,
            Item::Branch(b) => b.size(),
            Item::AlignP2(n) => {
                let align = 1usize << *n;
                (align - (off % align)) % align
            }
        }
    }
}

/// Emit `count` bytes of alignment padding using the same canonical multi-byte
/// NOP encodings LLVM-MC's `X86AsmBackend::writeNopData` uses in a code section
/// — required for byte-identity (a run of `0x90` would diverge). Lengths 1..=10
/// come straight from the table; 11..=15 prepend `count-10` `0x66` operand-size
/// prefixes to the 10-byte form. Pads longer than the max single NOP (15) are
/// split into successive NOPs, largest first.
fn write_nop_padding(code: &mut Vec<u8>, count: usize) {
    // Canonical NOPs by length (index = len-1).
    const NOPS: [&[u8]; 10] = [
        &[0x90],
        &[0x66, 0x90],
        &[0x0F, 0x1F, 0x00],
        &[0x0F, 0x1F, 0x40, 0x00],
        &[0x0F, 0x1F, 0x44, 0x00, 0x00],
        &[0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00],
        &[0x0F, 0x1F, 0x80, 0x00, 0x00, 0x00, 0x00],
        &[0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
        &[0x66, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
        &[0x66, 0x2E, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
    ];
    const MAX_NOP: usize = 15; // x86-64 generic: NOPs up to 15 bytes (66-prefixed).
    let mut remaining = count;
    while remaining != 0 {
        let this = remaining.min(MAX_NOP);
        let prefixes = this.saturating_sub(10);
        for _ in 0..prefixes {
            code.push(0x66);
        }
        let rest = this - prefixes;
        if rest != 0 {
            code.extend_from_slice(NOPS[rest - 1]);
        }
        remaining -= this;
    }
}

fn branch_for(mnemonic: &str, target: &str) -> Option<Branch> {
    if mnemonic == "call" {
        return Some(Branch { short: None, long: vec![0xE8], target: target.to_string(), is_long: true });
    }
    if mnemonic == "jmp" {
        return Some(Branch { short: Some(vec![0xEB]), long: vec![0xE9], target: target.to_string(), is_long: false });
    }
    if let Some(cc) = super::encode::jcc_nibble(mnemonic) {
        return Some(Branch {
            short: Some(vec![0x70 | cc]),
            long: vec![0x0F, 0x80 | cc],
            target: target.to_string(),
            is_long: false,
        });
    }
    None
}

/// Assemble a whole module's worth of text into an [`EncodedModule`], splitting
/// `.text` (code) and `.data` (read-write globals) into separate blobs.
pub fn assemble(text: &str) -> Result<EncodedModule> {
    Ok(assemble_impl(text, false)?.0)
}

/// Like [`assemble`], but also return a per-input-line byte span — `(start, len)`
/// into the final `code` for each line of `text` (`len == 0` for labels,
/// directives, blank lines). For the listing every byte lands in `code` (the
/// section split is flattened) so the spans index one contiguous blob, which is
/// what a listing view needs to show real branch displacements next to a line.
pub fn assemble_listing(text: &str) -> Result<(EncodedModule, Vec<(usize, usize)>)> {
    assemble_impl(text, true)
}

/// The shared driver. When `flatten`, `.data`/`.text` are ignored and every byte
/// goes into `code` (a single section, as the listing view wants); otherwise the
/// two sections are kept apart and a `.text`→`.data` reference becomes a reloc
/// the PE writer resolves once both sections have addresses.
fn assemble_impl(text: &str, flatten: bool) -> Result<(EncodedModule, Vec<(usize, usize)>)> {
    // ── Pass 1: parse into items (tracking each item's line + section) ──────
    let mut items: Vec<Item> = Vec::new();
    let mut item_line: Vec<usize> = Vec::new();
    let mut item_sect: Vec<Sect> = Vec::new();
    let mut cur = Sect::Text;
    for (lineno, raw) in text.lines().enumerate() {
        let start = items.len();
        // MC allows `label: insn` / `label: .quad ...` on one line; peel any
        // leading label into its own Item, then parse the remainder.
        let clean = super::parse::strip_comment(raw);
        let (label, rest) = super::parse::split_leading_label(clean);
        if let Some(name) = label {
            items.push(Item::Label(name.to_string()));
        }
        let label_only = label.is_some() && rest.is_empty();
        if !label_only {
            let body = if label.is_some() { rest } else { clean };
            let line = parse_line(body).with_context(|| format!("line {}: `{raw}`", lineno + 1))?;
            match line {
                Line::Empty => {}
                Line::Label(name) => items.push(Item::Label(name)),
                // Section switches change where following items land (no bytes).
                Line::Directive(Directive::Text) if !flatten => cur = Sect::Text,
                Line::Directive(Directive::Data) if !flatten => cur = Sect::Data,
                Line::Directive(d) => push_directive(&mut items, d)?,
                Line::Insn { mnemonic, ops } => {
                    // Relaxable branch/call to a symbol?
                    let branch = match ops.as_slice() {
                        [Operand::Sym(target)] => branch_for(&mnemonic, target),
                        _ => None,
                    };
                    if let Some(br) = branch {
                        items.push(Item::Branch(br));
                    } else {
                        let enc = encode(&mnemonic, &ops)
                            .with_context(|| format!("line {}: encode `{raw}`", lineno + 1))?;
                        let mut riprel = Vec::new();
                        for f in &enc.fixups {
                            match f.kind {
                                // `trailing` = bytes after the disp32 field within
                                // this instruction (an immediate, if any). RIP points
                                // at the instruction *end*, so the disp must reach
                                // past those bytes too.
                                FixupKind::RipRel32 => {
                                    let trailing = enc.bytes.len() - f.at - 4;
                                    riprel.push((f.at, trailing, f.target.clone(), f.disp));
                                }
                                FixupKind::Rel32 => {
                                    // A non-branch rel32 fixup shouldn't occur here.
                                    bail!("line {}: unexpected branch fixup in `{raw}`", lineno + 1);
                                }
                            }
                        }
                        items.push(Item::Code { bytes: enc.bytes, riprel });
                    }
                }
            }
        }
        for _ in start..items.len() {
            item_line.push(lineno);
            item_sect.push(cur);
        }
    }

    // ── Reject duplicate label definitions ─────────────────────────────────
    // Labels are module-scoped symbols resolved through one namespace (both
    // sections share it), so a second definition silently overwrites the first
    // in `layout` and redirects every branch/call/RIP-rel reference to it. Catch
    // it here, while `item_line` still maps each item back to its source line,
    // before relaxation and emission consume the (now-ambiguous) label map.
    let mut first_def: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (idx, it) in items.iter().enumerate() {
        if let Item::Label(n) = it {
            let line = item_line[idx] + 1;
            if let Some(&prev) = first_def.get(n.as_str()) {
                bail!("line {line}: duplicate label `{n}` (first defined on line {prev})");
            }
            first_def.insert(n.as_str(), line);
        }
    }

    // ── Pass 2: branch relaxation to a fixpoint ─────────────────────────────
    loop {
        let (places, labels) = layout(&items, &item_sect);
        let mut changed = false;
        for (it, &(sect, off)) in items.iter_mut().zip(&places) {
            if let Item::Branch(b) = it {
                if b.is_long {
                    continue;
                }
                // Extern or cross-section target → must be long; else fits-in-rel8?
                let must_long = match labels.get(&b.target) {
                    Some(&(tsect, tgt)) if tsect == sect => {
                        let after = off + b.size();
                        let disp = tgt as i64 - after as i64;
                        !(-128..=127).contains(&disp)
                    }
                    _ => true,
                };
                if must_long {
                    b.is_long = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // ── Pass 3: emit into the two section blobs ─────────────────────────────
    let (places, labels) = layout(&items, &item_sect);
    let mut code: Vec<u8> = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    let mut symbols: BTreeMap<String, usize> = BTreeMap::new();
    let mut data_symbols: BTreeMap<String, usize> = BTreeMap::new();
    let mut relocs: Vec<Reloc> = Vec::new();
    let mut externs: Vec<String> = Vec::new();
    let mut globls: std::collections::HashSet<String> = std::collections::HashSet::new();

    let nlines = text.lines().count();
    let mut spans: Vec<Option<(usize, usize)>> = vec![None; nlines];
    for (idx, it) in items.iter().enumerate() {
        let (sect, off) = places[idx];
        let line = item_line[idx];
        let buf = match sect {
            Sect::Text => &mut code,
            Sect::Data => &mut data,
        };
        debug_assert_eq!(off, buf.len());
        let s = buf.len();
        match it {
            Item::Label(n) => match sect {
                Sect::Text => {
                    symbols.insert(n.clone(), off);
                }
                Sect::Data => {
                    data_symbols.insert(n.clone(), off);
                }
            },
            Item::Globl(n) => {
                globls.insert(n.clone());
            }
            Item::AlignP2(n) => {
                let align = 1usize << *n;
                let pad = (align - (buf.len() % align)) % align;
                if sect == Sect::Text {
                    write_nop_padding(buf, pad); // canonical NOPs in code
                } else {
                    buf.resize(buf.len() + pad, 0); // zero-fill in data
                }
            }
            Item::Code { bytes, riprel } => {
                let base = buf.len();
                buf.extend_from_slice(bytes);
                for (at, trailing, target, fdisp) in riprel {
                    let field = base + at;
                    // RIP = instruction end = disp field end (+4) + any trailing
                    // immediate; carry that as a negative addend for the writers.
                    // The literal `+disp` rides in the disp32 field (and the
                    // same-section path folds it into the resolved displacement).
                    let addend = -(*trailing as i64);
                    match labels.get(target) {
                        // Same-section internal reference → resolve the disp now.
                        Some(&(tsect, tgt)) if tsect == sect => {
                            let disp = tgt as i64 + *fdisp as i64 - (field as i64 + 4 + *trailing as i64);
                            let d = i32::try_from(disp).context("RIP-rel disp32 overflow")?;
                            buf[field..field + 4].copy_from_slice(&d.to_le_bytes());
                        }
                        // Cross-section (code → data): emit a reloc the PE writer
                        // resolves against the data section's address.
                        Some(_) => relocs.push(Reloc {
                            at: field,
                            size: 4,
                            kind: RelocKind::RipRel32,
                            target: target.clone(),
                            addend,
                        }),
                        // Undefined here → an extern to bind.
                        None => {
                            relocs.push(Reloc {
                                at: field,
                                size: 4,
                                kind: RelocKind::RipRel32,
                                target: target.clone(),
                                addend,
                            });
                            externs.push(target.clone());
                        }
                    }
                }
            }
            Item::Branch(b) => {
                if b.is_long {
                    buf.extend_from_slice(&b.long);
                    let field = buf.len();
                    buf.extend_from_slice(&[0, 0, 0, 0]);
                    if let Some(&(_, tgt)) = labels.get(&b.target) {
                        let disp = tgt as i64 - (field as i64 + 4);
                        let d = i32::try_from(disp).context("branch rel32 overflow")?;
                        buf[field..field + 4].copy_from_slice(&d.to_le_bytes());
                    } else {
                        relocs.push(Reloc { at: field, size: 4, kind: RelocKind::BranchRel32, target: b.target.clone(), addend: 0 });
                        externs.push(b.target.clone());
                    }
                } else {
                    buf.extend_from_slice(b.short.as_ref().unwrap());
                    let field = buf.len();
                    buf.push(0);
                    let (_, tgt) = *labels.get(&b.target).expect("short branch to extern impossible");
                    let disp = tgt as i64 - (field as i64 + 1);
                    buf[field] = i8::try_from(disp).context("branch rel8 overflow")? as u8;
                }
            }
        }
        let e = buf.len();
        if e > s {
            let span = spans[line].get_or_insert((s, e));
            span.0 = span.0.min(s);
            span.1 = span.1.max(e);
        }
    }

    // Only export symbols that were .globl'd (others are module-local labels).
    symbols.retain(|name, _| globls.contains(name));
    externs.sort();
    externs.dedup();

    let listing = spans
        .into_iter()
        .map(|o| o.map_or((0, 0), |(s, e)| (s, e - s)))
        .collect();
    Ok((
        EncodedModule { code, data, symbols, data_symbols, relocs, externs },
        listing,
    ))
}

/// Compute each item's `(section, offset-within-section)` and the same for every
/// label. The two sections have independent offset counters.
fn layout(items: &[Item], sects: &[Sect]) -> (Vec<(Sect, usize)>, BTreeMap<String, (Sect, usize)>) {
    let mut places = Vec::with_capacity(items.len());
    let mut labels = BTreeMap::new();
    let (mut code_off, mut data_off) = (0usize, 0usize);
    for (it, &sect) in items.iter().zip(sects) {
        let off = match sect {
            Sect::Text => code_off,
            Sect::Data => data_off,
        };
        places.push((sect, off));
        if let Item::Label(n) = it {
            labels.insert(n.clone(), (sect, off));
        }
        let sz = it.size_at(off);
        match sect {
            Sect::Text => code_off += sz,
            Sect::Data => data_off += sz,
        }
    }
    (places, labels)
}

fn push_directive(items: &mut Vec<Item>, d: Directive) -> Result<()> {
    match d {
        // Section switches are handled in pass 1; reaching here is a no-op.
        Directive::IntelSyntax | Directive::Text | Directive::Data | Directive::Other(_) => {}
        Directive::Globl(n) => items.push(Item::Globl(n)),
        Directive::Quad(vs) => items.push(Item::Code {
            bytes: vs.iter().flat_map(|v| v.to_le_bytes()).collect(),
            riprel: vec![],
        }),
        Directive::Long(vs) => items.push(Item::Code {
            bytes: vs.iter().flat_map(|v| (*v as u32).to_le_bytes()).collect(),
            riprel: vec![],
        }),
        Directive::Word(vs) => items.push(Item::Code {
            bytes: vs.iter().flat_map(|v| (*v as u16).to_le_bytes()).collect(),
            riprel: vec![],
        }),
        Directive::Byte(b) => items.push(Item::Code { bytes: vec![b], riprel: vec![] }),
        Directive::Zero(n) => items.push(Item::Code { bytes: vec![0u8; n], riprel: vec![] }),
        Directive::Ascii(bytes, nul) => {
            let mut v = bytes;
            if nul {
                v.push(0);
            }
            items.push(Item::Code { bytes: v, riprel: vec![] });
        }
        Directive::Align(bytes) => {
            // byte alignment -> log2 (must be power of two)
            if !bytes.is_power_of_two() {
                bail!(".align {bytes} is not a power of two");
            }
            items.push(Item::AlignP2(bytes.trailing_zeros()));
        }
        Directive::P2align(n) => items.push(Item::AlignP2(n)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_call_and_short_branch_resolve() {
        // Two procs; an internal call (resolved, no reloc) and a short jcc.
        let src = "\
.text
.globl helper
helper:
mov rax, rcx
ret
.globl entry
entry:
test rax, rax
jz entry$$skip
call helper
entry$$skip:
ret
";
        let m = assemble(src).unwrap();
        // Both globls exported.
        assert!(m.symbols.contains_key("helper") && m.symbols.contains_key("entry"));
        // entry$$skip is local — not exported.
        assert!(!m.symbols.contains_key("entry$$skip"));
        // helper is internal → the `call helper` is resolved, NOT a reloc.
        assert!(m.relocs.is_empty(), "internal targets must not produce relocs: {:?}", m.relocs);
        assert!(m.externs.is_empty());
        // jz short form (74) present.
        assert!(m.code.windows(1).any(|w| w == [0x74]), "expected short jz (74) in {:02x?}", m.code);
    }

    #[test]
    fn extern_call_becomes_reloc() {
        let src = "\
.text
.globl w
w:
call rt_emit
ret
";
        let m = assemble(src).unwrap();
        assert_eq!(m.externs, vec!["rt_emit".to_string()]);
        assert_eq!(m.relocs.len(), 1);
        assert_eq!(m.relocs[0].kind, RelocKind::BranchRel32);
        assert_eq!(m.relocs[0].target, "rt_emit");
        // call is E8 rel32 (5 bytes) + ret.
        assert_eq!(m.code[0], 0xE8);
    }

    #[test]
    fn far_branch_relaxes_to_rel32() {
        // A forward jz over >127 bytes of filler must relax to the rel32 form.
        let mut src = String::from(".text\n.globl w\nw:\njz w$$end\n");
        for _ in 0..45 {
            src.push_str("mov rax, rcx\n"); // 3 bytes each = 135 bytes, well past rel8
        }
        src.push_str("w$$end:\nret\n");
        let m = assemble(&src).unwrap();
        // Long jcc form: 0F 8x.
        assert_eq!(&m.code[0..2], &[0x0F, 0x84], "expected near jz (0F 84): {:02x?}", &m.code[0..4]);
    }

    #[test]
    fn rip_relative_internal_and_extern() {
        let src = "\
.text
.globl w
w:
lea r8, [rip + helper]
lea r9, [rip + rt_emit]
ret
.globl helper
helper:
ret
";
        let m = assemble(src).unwrap();
        // helper is internal → patched; rt_emit → reloc.
        assert_eq!(m.relocs.len(), 1);
        assert_eq!(m.relocs[0].kind, RelocKind::RipRel32);
        assert_eq!(m.relocs[0].target, "rt_emit");
    }

    #[test]
    fn rip_relative_store_accounts_for_trailing_immediate() {
        // `mov dword ptr [rip+sym], imm32`: RIP points past the imm32, so the disp
        // must reach 4 bytes beyond the disp field. Regression — a missing
        // adjustment stored the value 4 bytes past `sym`. Cross-section/extern →
        // reloc carries the trailing length as a negative addend; same-section →
        // the disp is resolved with it baked in.
        let ext32 = assemble(".text\n.globl m\nm:\nmov dword ptr [rip + ext], 7\nret\n").unwrap();
        let r = ext32.relocs.iter().find(|r| r.target == "ext").unwrap();
        assert_eq!(r.kind, RelocKind::RipRel32);
        assert_eq!(r.addend, -4, "imm32 trailing the disp field → addend -4");

        let ext8 = assemble(".text\n.globl m\nm:\nmov byte ptr [rip + ext], 7\nret\n").unwrap();
        assert_eq!(ext8.relocs.iter().find(|r| r.target == "ext").unwrap().addend, -1);

        // No immediate (lea) → addend 0, unchanged.
        let lea = assemble(".text\n.globl m\nm:\nlea rax, [rip + ext]\nret\n").unwrap();
        assert_eq!(lea.relocs.iter().find(|r| r.target == "ext").unwrap().addend, 0);

        // Same-section internal store: C7 05 <disp32> <imm32> is 10 bytes and `sym`
        // sits right after, so disp = 10 - instruction_end(10) = 0 (the buggy value
        // was 4 — pointing the CPU at the imm32, not at `sym`).
        let int = assemble(".text\n.globl m\nm:\nmov dword ptr [rip + sym], 7\nsym:\nret\n").unwrap();
        let disp = i32::from_le_bytes([int.code[2], int.code[3], int.code[4], int.code[5]]);
        assert_eq!(disp, 0, "disp must reach the instruction end, not the disp field");
    }

    #[test]
    fn rip_relative_offset_is_carried_into_disp32() {
        // `[rip + sym + disp]` must fold `disp` in — dropping it is the bug that
        // nulled a struct-field store (`mov [rip+layer+20], 0` hit +0) and offset a
        // string base (`lea [rip+buf+9]` landed on +0).
        // Same-section LEA `48 8D 05 <disp32>` (7 bytes); `sym` follows at offset 7,
        // so disp = (sym=7 + 8) - rip_end(7) = 8.
        let m = assemble(".text\n.globl m\nm:\nlea rax, [rip + sym + 8]\nsym:\nret\n").unwrap();
        let disp = i32::from_le_bytes([m.code[3], m.code[4], m.code[5], m.code[6]]);
        assert_eq!(disp, 8, "the +8 must be folded into the disp32, not dropped");

        // Extern/cross-section: the offset rides in the disp32 field; the reloc
        // addend stays the trailing length (0 for a load) so PE/COFF add the field.
        let ext = assemble(".text\n.globl m\nm:\nmov eax, [rip + g + 20]\nret\n").unwrap();
        let f = i32::from_le_bytes([ext.code[2], ext.code[3], ext.code[4], ext.code[5]]);
        assert_eq!(f, 20, "extern operand keeps +20 in the disp32 field");
        assert_eq!(ext.relocs.iter().find(|r| r.target == "g").unwrap().addend, 0);
    }

    #[test]
    fn listing_spans_map_lines_to_bytes() {
        let src = ".text\n.globl w\nw:\nmov rax, rcx\nret\n";
        let (m, spans) = assemble_listing(src).unwrap();
        assert_eq!(m.code, assemble(src).unwrap().code, "same bytes as assemble()");
        assert_eq!(spans.len(), src.lines().count(), "one span per input line");
        // Directive/label lines contribute no bytes.
        assert_eq!((spans[0], spans[1], spans[2]), ((0, 0), (0, 0), (0, 0)));
        // `mov rax, rcx` is 3 bytes at offset 0; `ret` is 1 byte right after.
        assert_eq!(spans[3], (0, 3));
        assert_eq!(spans[4], (3, 1));
    }

    #[test]
    fn data_section_splits_and_cross_ref_is_a_reloc() {
        // A `.data` global, written through its address from `.text`.
        let src = "\
.data
counter:
.long 0
.text
.globl main
main:
lea rcx, [rip + counter]
mov dword ptr [rcx], 42
ret
";
        let m = assemble(src).unwrap();
        // The `.long 0` landed in `.data`, not `.text`; `main` starts at code 0.
        assert_eq!(m.data, vec![0, 0, 0, 0], "data byte in .data blob");
        assert_eq!(m.data_symbols.get("counter"), Some(&0));
        assert_eq!(m.symbols.get("main"), Some(&0), "code starts with main");
        // The `.text`→`.data` reference is a reloc the PE writer resolves, and
        // `counter` is internal — not an extern.
        assert_eq!(m.relocs.len(), 1);
        assert_eq!(m.relocs[0].kind, RelocKind::RipRel32);
        assert_eq!(m.relocs[0].target, "counter");
        assert!(m.externs.is_empty(), "internal data label must not be an extern");
    }

    #[test]
    fn listing_flattens_sections_into_one_blob() {
        // The listing view ignores the section split so its spans index a single
        // `code` blob and a cross reference is patched in place (no reloc).
        let src = ".data\ncounter:\n.long 7\n.text\n.globl w\nw:\nlea rcx, [rip + counter]\nret\n";
        let (m, spans) = assemble_listing(src).unwrap();
        assert!(m.data.is_empty(), "listing keeps every byte in code");
        assert!(m.relocs.is_empty(), "cross-ref is patched in flat mode");
        assert_eq!(spans.len(), src.lines().count());
        assert_eq!(spans[2], (0, 4), "`.long 7` is 4 bytes at code offset 0");
    }

    #[test]
    fn listing_resolves_internal_branch_displacement() {
        // A short forward jcc to a local label: the listed bytes carry the real
        // rel8 displacement, not a zero placeholder.
        let src = ".globl w\nw:\ntest rax, rax\njz done\nnop\ndone:\nret\n";
        let (m, spans) = assemble_listing(src).unwrap();
        let (s, len) = spans[3]; // `jz done`
        assert_eq!(len, 2, "short jz is `74 disp8`: {spans:?}");
        assert_eq!(m.code[s], 0x74, "short jz opcode");
        assert_eq!(m.code[s + 1], 0x01, "disp8 jumps over the 1-byte nop");
        assert!(m.relocs.is_empty(), "all-internal module has no relocs");
    }

    #[test]
    fn duplicate_labels_are_rejected() {
        // Two `foo:` on their own lines — the second silently overwrote the first
        // before this check, redirecting every reference to it.
        let err = assemble(".text\nfoo:\nret\nfoo:\nret\n").unwrap_err().to_string();
        assert!(err.contains("duplicate label `foo`"), "{err}");
        assert!(err.contains("first defined on line 2"), "{err}");

        // Inline `label: insn` form is peeled into its own label Item, so it must
        // be caught the same way.
        let err = assemble(".text\nfoo: ret\nfoo: ret\n").unwrap_err().to_string();
        assert!(err.contains("duplicate label `foo`"), "{err}");

        // Cross-section duplicates collide too — labels share one namespace.
        let err = assemble(".data\nx:\n.long 0\n.text\nx:\nret\n").unwrap_err().to_string();
        assert!(err.contains("duplicate label `x`"), "{err}");
    }
}
