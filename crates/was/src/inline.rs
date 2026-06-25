//! Small-proc inliner — a deferred-assembly reducer over the lowered asm text.
//!
//! Modelled on WF66's level-2 instruction-buffer reducer (docs/design/
//! wf66_dual_level_reducer.md): lex the lowered asm into a line buffer, reduce it
//! with recognize→replace passes, re-render. The buffer's contract is an identity
//! round-trip — `render(parse(x)) == x` — so introducing it changes nothing until
//! a pass is added. Only what a pass needs is structured; every other line rides as
//! `Other` (verbatim) and is a barrier passes never reason across.
//!
//! Two-sources-of-truth safety (WF66 §1): a `call` is only ever *replaced* when the
//! callee is fully understood (small, leaf, unframed, single-region, address never
//! taken). Anything uncertain stays a `call` — the build is correct no matter what,
//! so inlining is pure upside.
//!
//! Pipeline (each step is a separate pass, run to a fixpoint):
//!   parse → inline_calls → dce_after_jump → dead_store → render
//! Phase 1 lands the buffer + the identity round-trip; later commits add the passes.

use std::collections::{HashMap, HashSet};

/// One lowered line: its verbatim text (so render is exact) plus a `Kind` the
/// passes match on. A pass that rewrites a line replaces both.
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    /// Matched by the inline/cleanup passes (which land on top of this buffer).
    #[allow(dead_code)]
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `name:` — a label definition.
    Label(String),
    /// `call <label>` — a direct call (not `call [rax]` / `call reg`).
    Call(String),
    /// `jmp`/`jCC`/`loop` `<label>` — a branch to a label (for per-site renaming).
    Jump(String),
    /// `ret`.
    Ret,
    /// `; @wfi NAME C` — the lowerer's proc-span opener (right after the label).
    /// `complex` (the `C` flag = 1) marks a framed/xmm-saving proc we won't splice.
    ProcMark { name: String, complex: bool },
    /// `; @wfi-end NAME` — the proc-span closer (right after the final epilogue).
    EndMark(String),
    /// Anything else, verbatim — comments, instructions, indirect calls, data.
    Other,
}

/// True if `s` is a single bare label operand (not memory, not a register, not an
/// immediate) — the target of a `call`/`jmp` we can rename when splicing.
fn is_label_operand(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && !s.contains('[')
        && !s.contains(',')
        && !s.contains(char::is_whitespace)
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '.')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
        && gp_or_xmm_reg(s).is_none()
}

/// Recognize a register name so a `call rax` / `jmp rdx` indirect target isn't
/// mistaken for a label.
fn gp_or_xmm_reg(s: &str) -> Option<()> {
    const REGS: &[&str] = &[
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15", "eax", "ebx", "ecx", "edx", "esi", "edi", "ax", "al",
    ];
    REGS.contains(&s.to_ascii_lowercase().as_str()).then_some(())
}

/// A conditional/unconditional branch mnemonic (whose operand is a label target).
fn is_branch_mnemonic(mn: &str) -> bool {
    matches!(
        mn,
        "jmp" | "loop" | "loope" | "loopne" | "jecxz" | "jrcxz"
    ) || (mn.starts_with('j') && mn.len() >= 2 && mn.len() <= 4)
}

/// Classify a lowered line. Conservative: only the shapes the passes act on are
/// recognized; everything else is `Other` (a barrier).
fn classify(line: &str) -> Kind {
    let t = line.trim();
    if t.is_empty() || t.starts_with(';') {
        if let Some(rest) = t.strip_prefix("; @wfi-end ") {
            return Kind::EndMark(rest.trim().to_string());
        }
        if let Some(rest) = t.strip_prefix("; @wfi ") {
            let mut it = rest.split_whitespace();
            if let Some(name) = it.next() {
                return Kind::ProcMark {
                    name: name.to_string(),
                    complex: it.next() == Some("1"),
                };
            }
        }
        return Kind::Other;
    }
    if let Some(name) = t.strip_suffix(':') {
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
        {
            return Kind::Label(name.to_string());
        }
    }
    let (mn, rest) = match t.split_once(char::is_whitespace) {
        Some((m, r)) => (m.to_ascii_lowercase(), r.trim()),
        None => (t.to_ascii_lowercase(), ""),
    };
    if mn == "ret" && rest.is_empty() {
        return Kind::Ret;
    }
    if mn == "call" && is_label_operand(rest) {
        return Kind::Call(rest.to_string());
    }
    if is_branch_mnemonic(&mn) && is_label_operand(rest) {
        return Kind::Jump(rest.to_string());
    }
    Kind::Other
}

/// Lex lowered asm text into the line buffer.
pub fn parse(asm: &str) -> Vec<Line> {
    asm.lines()
        .map(|l| Line {
            text: l.to_string(),
            kind: classify(l),
        })
        .collect()
}

/// Render the buffer back to asm text — the inverse of [`parse`] for the lowered
/// text the lowerer produces (each line '\n'-terminated): `render(parse(x)) == x`.
pub fn render(lines: &[Line]) -> String {
    let mut s = String::with_capacity(lines.iter().map(|l| l.text.len() + 1).sum());
    for l in lines {
        s.push_str(&l.text);
        s.push('\n');
    }
    s
}

/// A proc's span in the line buffer, from its `; @wfi` opener marker (right after
/// the label) to its `; @wfi-end` closer.
struct ProcSpan {
    name: String,
    procmark_idx: usize, // the `; @wfi` line (right after the `name:` label)
    endmark_idx: usize,  // the `; @wfi-end` line
    complex: bool,       // framed / xmm-saving — never inlined
}

/// The proc body to splice: prologue + body + epilogue + ret(s), i.e. everything
/// between the two markers (excludes the label and the markers themselves).
fn body_slice<'a>(lines: &'a [Line], s: &ProcSpan) -> &'a [Line] {
    if s.endmark_idx > s.procmark_idx + 1 {
        &lines[s.procmark_idx + 1..s.endmark_idx]
    } else {
        &[]
    }
}

/// Pair each `; @wfi`/`; @wfi-end` into a span.
fn collect_procs(lines: &[Line]) -> Vec<ProcSpan> {
    let mut spans = Vec::new();
    let mut open: Option<(String, usize, bool)> = None;
    for (i, l) in lines.iter().enumerate() {
        match &l.kind {
            Kind::ProcMark { name, complex } => {
                open = Some((name.clone(), i, *complex));
            }
            Kind::EndMark(name) => {
                if let Some((n, procmark_idx, complex)) = open.take() {
                    if &n == name {
                        spans.push(ProcSpan { name: n, procmark_idx, endmark_idx: i, complex });
                    }
                }
            }
            _ => {}
        }
    }
    spans
}

/// A line that emits a real instruction (for sizing — not a label/marker/comment).
fn is_instruction(l: &Line) -> bool {
    match &l.kind {
        Kind::Label(_) | Kind::ProcMark { .. } | Kind::EndMark(_) => false,
        Kind::Other => {
            let t = l.text.trim();
            !t.is_empty() && !t.starts_with(';')
        }
        Kind::Call(_) | Kind::Jump(_) | Kind::Ret => true,
    }
}

/// A proc is inlinable if it's a plain unframed leaf of at most `threshold`
/// instructions. (Leaf = no nested call, so no recursion and no nested frames.)
fn is_eligible(lines: &[Line], s: &ProcSpan, threshold: usize) -> bool {
    if s.complex {
        return false;
    }
    let body = body_slice(lines, s);
    let mut ninstr = 0usize;
    for l in body {
        if matches!(l.kind, Kind::Call(_)) {
            return false; // not a leaf
        }
        if is_instruction(l) {
            ninstr += 1;
        }
    }
    ninstr > 0 && ninstr <= threshold
}

/// True if `name` is referenced other than by a `call` — jumped to, address-taken
/// (appears as a bare token in some non-call line), or exported with `.globl`. Such
/// a proc's definition must stay even after all its calls are inlined.
fn referenced_elsewhere(lines: &[Line], name: &str) -> bool {
    for l in lines {
        match &l.kind {
            Kind::Jump(t) if t == name => return true,
            Kind::Label(_) | Kind::Call(_) | Kind::ProcMark { .. } | Kind::EndMark(_) | Kind::Ret => {}
            _ => {
                if contains_word(&l.text, name) {
                    return true;
                }
            }
        }
    }
    false
}

/// `name` appears as a whole identifier token in `text`.
fn contains_word(text: &str, name: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$'))
        .any(|w| w == name)
}

fn leading_ws(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

fn split_mnemonic(s: &str) -> (String, String) {
    let t = s.trim_start();
    match t.split_once(char::is_whitespace) {
        Some((m, r)) => (m.to_string(), r.trim().to_string()),
        None => (t.to_string(), String::new()),
    }
}

/// Splice a proc body at a call site (`site` = a unique id): rename the body-local
/// labels with a per-site suffix (so two inlinings don't collide), and turn every
/// `ret` into a jump to a fresh continuation label appended at the end (so an early
/// `ret` / the explicit-ret double epilogue can't return out of the caller).
fn splice_body(body: &[Line], site: usize) -> Vec<Line> {
    let cont = format!("__wfi_cont_{site}");
    let locals: HashSet<&str> = body
        .iter()
        .filter_map(|l| match &l.kind {
            Kind::Label(n) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    let mut out = Vec::with_capacity(body.len() + 1);
    for l in body {
        match &l.kind {
            Kind::Ret => out.push(Line {
                text: format!("  jmp {cont}"),
                kind: Kind::Jump(cont.clone()),
            }),
            Kind::Label(n) if locals.contains(n.as_str()) => {
                let nn = format!("{n}__wfi{site}");
                out.push(Line {
                    text: format!("{}{nn}:", leading_ws(&l.text)),
                    kind: Kind::Label(nn),
                });
            }
            Kind::Jump(t) if locals.contains(t.as_str()) => {
                let nt = format!("{t}__wfi{site}");
                let (mn, _) = split_mnemonic(&l.text);
                out.push(Line {
                    text: format!("{}{mn} {nt}", leading_ws(&l.text)),
                    kind: Kind::Jump(nt),
                });
            }
            _ => out.push(l.clone()),
        }
    }
    out.push(Line {
        text: format!("{cont}:"),
        kind: Kind::Label(cont),
    });
    out
}

/// Peephole: a `jmp X` immediately followed by `X:` is a no-op — drop the jump.
/// (Cleans the continuation jump a tail-position `ret` splices to.)
fn drop_jmp_to_next(lines: Vec<Line>, origin: Vec<usize>) -> (Vec<Line>, Vec<usize>) {
    let mut out = Vec::with_capacity(lines.len());
    let mut oorigin = Vec::with_capacity(origin.len());
    let mut i = 0;
    while i < lines.len() {
        if let Kind::Jump(t) = &lines[i].kind {
            if let Some(Kind::Label(l)) = lines.get(i + 1).map(|n| &n.kind) {
                if t == l {
                    i += 1; // drop the jmp; the label is emitted next iteration
                    continue;
                }
            }
        }
        out.push(lines[i].clone());
        oorigin.push(origin[i]);
        i += 1;
    }
    (out, oorigin)
}

/// A `__wfi_cont_N:` label that nothing jumps to is dead (a single-exit proc's
/// continuation, after its tail `ret`→jmp was peepholed). Strip those — they're
/// the bulk of an inlined region's residue. (Only our own labels, only when no
/// jump targets them, so it's always sound.)
fn remove_dead_cont_labels(lines: Vec<Line>, origin: Vec<usize>) -> (Vec<Line>, Vec<usize>) {
    let targets: HashSet<&str> = lines
        .iter()
        .filter_map(|l| match &l.kind {
            Kind::Jump(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut oorigin = Vec::with_capacity(origin.len());
    for (l, o) in lines.iter().zip(origin) {
        if let Kind::Label(n) = &l.kind {
            if n.starts_with("__wfi_cont_") && !targets.contains(n.as_str()) {
                continue; // dead continuation label — drop it
            }
        }
        out.push(l.clone());
        oorigin.push(o);
    }
    (out, oorigin)
}

/// `pop R` and `push R` operand, if the line is exactly that (a single bare reg).
fn pop_reg(l: &Line) -> Option<&str> {
    bare_unary(&l.text, "pop")
}
fn push_reg(l: &Line) -> Option<&str> {
    bare_unary(&l.text, "push")
}
fn bare_unary<'a>(text: &'a str, mn: &str) -> Option<&'a str> {
    let t = text.trim();
    let rest = t.strip_prefix(mn)?.strip_prefix(char::is_whitespace)?.trim();
    (gp_or_xmm_reg(rest).is_some()).then_some(rest)
}

/// A no-op line between a `pop` and a `push` we can see through (comment/blank).
fn is_transparent(l: &Line) -> bool {
    matches!(l.kind, Kind::Other) && {
        let t = l.text.trim();
        t.is_empty() || t.starts_with(';')
    }
}

/// After a `push R` (the value just re-saved), is R overwritten before it's read?
/// Forward scan, conservative: a read keeps it live, a write kills it, any barrier
/// (label/call/jump/ret) stops the scan as live. Sound regardless of contracts.
fn written_before_read(lines: &[Line], push_idx: usize, r: &str) -> bool {
    let Some(rc) = crate::gp_reg(r) else { return false };
    for l in &lines[push_idx + 1..] {
        match &l.kind {
            Kind::Label(_) | Kind::Call(_) | Kind::Jump(_) | Kind::Ret => return false,
            _ => {
                let (reads, writes) = crate::reg_effects(l.text.trim(), &crate::gp_reg);
                if reads.contains(&rc) {
                    return false;
                }
                if writes.contains(&rc) {
                    return true;
                }
            }
        }
    }
    false
}

/// Is `r` a callee-saved GP register — what a `uses` prologue saves with push/pop?
fn is_callee_saved(r: &str) -> bool {
    matches!(
        crate::gp_reg(r),
        Some("rbx" | "rbp" | "rsi" | "rdi" | "r12" | "r13" | "r14" | "r15")
    )
}

/// The general "elide if proven safe" framing reducer: for each inlined region,
/// elide a prologue `push R` / epilogue `pop R` pair when R is provably dead after
/// the restore (written-before-read forward scan). The leading pushes and trailing
/// pops must form a clean LIFO of callee-saved registers (so they're the frame, not
/// body stack traffic); each matched pair is then independently stack-balanced to
/// remove. Catches e.g. an inlined call in tail position whose `pop R` is at once
/// overwritten by the enclosing proc's own epilogue restore of R.
fn elide_dead_frames(lines: Vec<Line>, origin: Vec<usize>) -> (Vec<Line>, Vec<usize>) {
    let mut drop = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].text.contains("\u{2500}\u{2500} inline") {
            i += 1;
            continue;
        }
        let Some(end) = (i + 1..lines.len()).find(|&k| lines[k].text.contains("\u{2500}\u{2500} end"))
        else {
            i += 1;
            continue;
        };
        // leading prologue pushes (callee-saved), comments allowed between
        let mut pushes: Vec<(usize, &str)> = Vec::new();
        let mut k = i + 1;
        while k < end {
            if is_transparent(&lines[k]) {
                k += 1;
                continue;
            }
            match push_reg(&lines[k]) {
                Some(r) if is_callee_saved(r) => {
                    pushes.push((k, r));
                    k += 1;
                }
                _ => break,
            }
        }
        // trailing epilogue pops (callee-saved), scanning back from the end marker
        let mut pops: Vec<(usize, &str)> = Vec::new();
        let mut m = end;
        while m > i + 1 {
            m -= 1;
            if is_transparent(&lines[m]) {
                continue;
            }
            match pop_reg(&lines[m]) {
                Some(r) if is_callee_saved(r) => pops.push((m, r)),
                _ => break,
            }
        }
        // clean LIFO frame? pushes [r0,r1,..]; pops collected end→start are also
        // [r0,r1,..]; pair pushes[j] with pops[j] (same register) and prove dead.
        let push_regs: Vec<&str> = pushes.iter().map(|(_, r)| *r).collect();
        let pop_regs: Vec<&str> = pops.iter().map(|(_, r)| *r).collect();
        if !push_regs.is_empty() && push_regs == pop_regs {
            for (j, &(pidx, r)) in pushes.iter().enumerate() {
                let (qidx, _) = pops[j];
                if written_before_read(&lines, qidx, r) {
                    drop[pidx] = true;
                    drop[qidx] = true;
                }
            }
        }
        i = end + 1;
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut oorigin = Vec::with_capacity(origin.len());
    for (k, (l, o)) in lines.iter().zip(origin).enumerate() {
        if !drop[k] {
            out.push(l.clone());
            oorigin.push(o);
        }
    }
    (out, oorigin)
}

/// Run the inliner over lowered asm. Returns the rewritten text plus an `origin`
/// map (`origin[i]` = the 0-based input line output line `i` derives from), so the
/// caller can keep error line-mapping intact. Markers are always stripped; a call
/// to an eligible proc is spliced; a proc whose every call was inlined and which is
/// referenced nowhere else has its definition removed (DCE).
pub fn optimize(asm: &str, threshold: usize) -> (String, Vec<usize>) {
    let lines = parse(asm);
    let procs = collect_procs(&lines);
    if procs.is_empty() {
        return (render(&lines), (0..lines.len()).collect());
    }
    let eligible: HashSet<&str> = procs
        .iter()
        .filter(|s| is_eligible(&lines, s, threshold))
        .map(|s| s.name.as_str())
        .collect();
    // An eligible proc has all its calls inlined → its def is dead unless something
    // other than a call still reaches it.
    let dce: HashSet<&str> = eligible
        .iter()
        .copied()
        .filter(|n| !referenced_elsewhere(&lines, n))
        .collect();
    let span_of: HashMap<&str, &ProcSpan> = procs.iter().map(|s| (s.name.as_str(), s)).collect();

    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut origin: Vec<usize> = Vec::with_capacity(lines.len());
    let mut site = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        // strip span markers
        if matches!(line.kind, Kind::ProcMark { .. } | Kind::EndMark(_)) {
            i += 1;
            continue;
        }
        // skip a dead proc definition wholesale (label .. endmark)
        if let Kind::Label(name) = &line.kind {
            if dce.contains(name.as_str()) {
                if let Some(s) = span_of.get(name.as_str()) {
                    i = s.endmark_idx + 1;
                    continue;
                }
            }
        }
        // splice an eligible call
        if let Kind::Call(name) = &line.kind {
            if eligible.contains(name.as_str()) {
                let s = span_of[name.as_str()];
                site += 1;
                out.push(Line { text: format!("  ; \u{2500}\u{2500} inline {name}"), kind: Kind::Other });
                origin.push(i);
                for sl in splice_body(body_slice(&lines, s), site) {
                    out.push(sl);
                    origin.push(i);
                }
                out.push(Line { text: format!("  ; \u{2500}\u{2500} end {name}"), kind: Kind::Other });
                origin.push(i);
                i += 1;
                continue;
            }
        }
        out.push(line.clone());
        origin.push(i);
        i += 1;
    }
    let (mut out, mut origin) = drop_jmp_to_next(out, origin);
    (out, origin) = remove_dead_cont_labels(out, origin);
    // Elide every framing pair we can prove dead, to a fixpoint: eliding one
    // region's frame can make a neighbour's register provably dead too.
    for _ in 0..4 {
        let before = out.len();
        (out, origin) = elide_dead_frames(out, origin);
        if out.len() == before {
            break;
        }
    }
    (render(&out), origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_identity() {
        let samples = [
            "Helper:\n  push rbx\n mov rbx, rcx\nloop:\n dec rbx\n jnz loop\n  pop rbx\n  ret\n",
            ".code\n  call Foo\n  mov rax, 1\n  ret\n",
            "; a comment\nL1:\n  jmp L1\n",
            "  call [rax]\n  call rdx\n", // indirect calls stay Other
        ];
        for s in samples {
            assert_eq!(render(&parse(s)), s, "round-trip must be identity for:\n{s}");
            let (out, origin) = optimize(s, 6);
            assert_eq!(out, s, "optimize identity for:\n{s}");
            assert_eq!(origin, (0..s.lines().count()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn inlines_a_leaf_proc_with_per_site_labels() {
        // Foo: a small unframed leaf with an internal loop, called twice from Bar
        // (Bar is framed → complex=1 → never inlined, always kept).
        let asm = "Foo:\n; @wfi Foo 0\n  push rbx\n  mov rbx, rcx\nfloop:\n  dec rbx\n  jnz floop\n  pop rbx\n  ret\n; @wfi-end Foo\nBar:\n; @wfi Bar 1\n  mov rcx, 5\n  call Foo\n  call Foo\n  ret\n; @wfi-end Bar\n";
        let (out, origin) = optimize(asm, 8);
        assert_eq!(origin.len(), out.lines().count(), "origin maps every output line");
        assert!(!out.contains("@wfi"), "markers stripped:\n{out}");
        assert!(!out.contains("call Foo"), "both calls inlined:\n{out}");
        assert!(!out.lines().any(|l| l.trim() == "Foo:"), "dead Foo def DCE'd:\n{out}");
        assert!(out.lines().any(|l| l.trim() == "Bar:"), "framed Bar kept:\n{out}");
        // internal label uniquified per site; its backward jump renamed to match
        assert!(out.contains("floop__wfi1:") && out.contains("floop__wfi2:"), "per-site labels:\n{out}");
        assert!(out.contains("jnz floop__wfi1") && out.contains("jnz floop__wfi2"), "renamed jumps:\n{out}");
        // cleanup: dead continuation labels stripped. Both framings stay here —
        // the elision is conservative: rbx is live across each pop (the next body's
        // push / Bar's ret), so neither pair is *proven* dead.
        assert!(!out.contains("__wfi_cont"), "dead continuation labels removed:\n{out}");
        assert_eq!(out.matches("push rbx").count(), 2, "both framings kept (rbx live):\n{out}");
        assert_eq!(out.matches("pop rbx").count(), 2, "both framings kept (rbx live):\n{out}");
        assert!(out.contains("inline Foo") && out.contains("end Foo"), "visible inline markers:\n{out}");
    }

    #[test]
    fn dead_continuation_labels_removed() {
        // a `__wfi_cont` label nothing jumps to is dropped; a jumped-to one stays
        let labs = parse("  jmp __wfi_cont_5\n__wfi_cont_5:\n__wfi_cont_9:\n");
        let (c, _) = remove_dead_cont_labels(labs, (0..3).collect());
        let txt = render(&c);
        assert!(txt.contains("__wfi_cont_5:"), "live cont kept:\n{txt}");
        assert!(!txt.contains("__wfi_cont_9:"), "dead cont removed:\n{txt}");
    }

    #[test]
    fn elide_dead_frames_when_provably_dead() {
        // an inlined region whose `pop rbx` is at once overwritten by the enclosing
        // proc's own epilogue `pop rbx` → the framing pair is dead → elided.
        let dead = parse("  ; \u{2500}\u{2500} inline Foo\n  push rbx\n  mov rbx, 7\n  pop rbx\n  ; \u{2500}\u{2500} end Foo\n  pop rbx\n  ret\n");
        let (out, _) = elide_dead_frames(dead, (0..7).collect());
        let txt = render(&out);
        assert_eq!(txt.matches("push rbx").count(), 0, "dead frame push elided:\n{txt}");
        assert_eq!(txt.matches("pop rbx").count(), 1, "only the enclosing epilogue pop remains:\n{txt}");

        // but kept when the restored value is read afterwards (live)
        let live = parse("  ; \u{2500}\u{2500} inline Foo\n  push rbx\n  mov rbx, 7\n  pop rbx\n  ; \u{2500}\u{2500} end Foo\n  mov rax, rbx\n  ret\n");
        let (out2, _) = elide_dead_frames(live, (0..7).collect());
        let txt2 = render(&out2);
        assert_eq!(txt2.matches("push rbx").count(), 1, "live frame kept:\n{txt2}");
    }

    #[test]
    fn classify_recognizes_the_shapes() {
        assert_eq!(classify("Foo:"), Kind::Label("Foo".into()));
        assert_eq!(classify("  call Helper"), Kind::Call("Helper".into()));
        assert_eq!(classify("  jnz loop"), Kind::Jump("loop".into()));
        assert_eq!(classify("  ret"), Kind::Ret);
        // not labels/calls we can splice:
        assert_eq!(classify("  call [rax]"), Kind::Other);
        assert_eq!(classify("  call rdx"), Kind::Other);
        assert_eq!(classify("  mov rax, rbx"), Kind::Other);
        assert_eq!(classify("; @marker"), Kind::Other);
    }
}
