//! Windows assembler front-end. Lowers a thin superset of Intel asm to the plain
//! text rasm assembles, resolving the Windows surface through [`winkb`]:
//!
//! * `invoke Func, a0, a1, …` — Win64-ABI marshaling (shadow space, arg→register,
//!   16-byte alignment) + `call Func`. The call stays an extern for the linker.
//! * bare constant / enum names in operands — `mov ecx, MB_OK` → `mov ecx, 0`.
//! * `Struct.field` — the field's byte offset; `sizeof(Type)` — the type's size.
//!
//! Resolution is lazy and conservative: an identifier is only rewritten when it
//! is not a register, not a label defined in this source, and winkb knows it —
//! otherwise it is left for rasm to treat as a label/extern.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use winkb::Kb;

/// First four integer/pointer argument registers (Win64).
const ARG_REGS: [&str; 4] = ["rcx", "rdx", "r8", "r9"];

/// How serious a diagnostic is. The IDE colours by this: Info → blue,
/// Warn → yellow/amber, Error → red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// One diagnostic: a 1-based line/column, a message, and a severity.
#[derive(Debug, Clone)]
pub struct Diag {
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub severity: Severity,
}

/// One of the six caller-saved general registers an `invoke`/`call`/COM call
/// destroys (rax is excluded: it's the *return value*, so using it after a call
/// is the idiom, not a bug; xmm is left out to stay conservative).
fn clobber_reg(s: &str) -> Option<&'static str> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "rcx" | "ecx" | "cx" | "cl" | "ch" => "rcx",
        "rdx" | "edx" | "dx" | "dl" | "dh" => "rdx",
        "r8" | "r8d" | "r8w" | "r8b" => "r8",
        "r9" | "r9d" | "r9w" | "r9b" => "r9",
        "r10" | "r10d" | "r10w" | "r10b" => "r10",
        "r11" | "r11d" | "r11w" | "r11b" => "r11",
        _ => return None,
    })
}

const CLOBBER_REGS: [&str; 6] = ["rcx", "rdx", "r8", "r9", "r10", "r11"];

/// The callee-saved general registers — the ones a subroutine must preserve for
/// its caller (rsp is excluded; it's the stack pointer, managed separately).
fn nonvol_reg(s: &str) -> Option<&'static str> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "rbx" | "ebx" | "bx" | "bl" | "bh" => "rbx",
        "rbp" | "ebp" | "bp" | "bpl" => "rbp",
        "rsi" | "esi" | "si" | "sil" => "rsi",
        "rdi" | "edi" | "di" | "dil" => "rdi",
        "r12" | "r12d" | "r12w" | "r12b" => "r12",
        "r13" | "r13d" | "r13w" | "r13b" => "r13",
        "r14" | "r14d" | "r14w" | "r14b" => "r14",
        "r15" | "r15d" | "r15w" | "r15b" => "r15",
        _ => return None,
    })
}

/// The callee-saved vector registers on Win64 — xmm6..xmm15 (and the ymm/zmm that
/// alias them: writing ymm6 destroys the preserved low 128 bits of xmm6, so we
/// canonicalize any width to its xmm name). A subroutine must preserve these for
/// its caller; plain `proc`/`uses` can't (it only `push`es GP registers), which is
/// what `fproc` exists to handle.
fn nonvol_xmm(s: &str) -> Option<&'static str> {
    let s = s.trim().to_ascii_lowercase();
    let n = s
        .strip_prefix("xmm")
        .or_else(|| s.strip_prefix("ymm"))
        .or_else(|| s.strip_prefix("zmm"))?;
    Some(match n {
        "6" => "xmm6",
        "7" => "xmm7",
        "8" => "xmm8",
        "9" => "xmm9",
        "10" => "xmm10",
        "11" => "xmm11",
        "12" => "xmm12",
        "13" => "xmm13",
        "14" => "xmm14",
        "15" => "xmm15",
        _ => return None,
    })
}

/// A store of a non-volatile xmm to memory (`movups [rsp+16], xmm6` and friends) —
/// a hand-rolled save. Lets the contract check tell a preserved clobber from a lost
/// one in a plain `proc` (an `fproc` saves them for you via `uses`).
fn xmm_save_target(t: &str) -> Option<&'static str> {
    let (mn, rest) = match t.find(char::is_whitespace) {
        Some(p) => (t[..p].to_ascii_lowercase(), t[p..].trim()),
        None => return None,
    };
    let is_move = matches!(
        mn.as_str(),
        "movups" | "movaps" | "movupd" | "movapd" | "movdqu" | "movdqa" | "movsd" | "movss" | "movq" | "movd"
    ) || mn.starts_with("vmov");
    if !is_move {
        return None;
    }
    let ops = split_top_commas(rest);
    if ops.len() < 2 || !ops[0].contains('[') {
        return None; // not a store to memory
    }
    nonvol_xmm(&ops[1])
}

/// A bare integer immediate (`42` / `0x10`), for tracking `sub rsp, N`.
fn simple_imm(s: &str) -> Option<i64> {
    let s = s.trim();
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .and_then(|h| i64::from_str_radix(h, 16).ok())
        .or_else(|| s.parse::<i64>().ok())
}

/// A label line (`name:`) — a block boundary for the rsp-balance reset.
fn is_label_line(t: &str) -> bool {
    t.strip_suffix(':')
        .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$'))
}

/// Any call (external or a plain `call <label>`) — where a framed proc's stack
/// must sit at the aligned frame level.
fn is_any_call(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    is_call_site(t) || l == "call" || l.starts_with("call ") || l.starts_with("call\t")
}

/// Enforce the `proc … endproc` contract — three checks, each the dual of the
/// caller-side analysis:
///   * **uses**: the body may not modify a callee-saved register outside `uses`
///     (else the caller's value is silently lost);
///   * **in/out**: a register read but never set and not declared `in` is an
///     uninitialized input; an `out` register never written is an unkept promise;
///   * **frame balance**: inside a `frame` proc the stack must be at the aligned
///     frame level at every call — a stray `push`/`sub rsp` would break it.
pub fn proc_contract_diags(src: &str) -> Vec<Diag> {
    struct Acc {
        name: String,
        line: usize,
        fp: bool, // an `fproc`: its `uses` may save xmm6..xmm15
        saved: Vec<&'static str>,
        saved_xmm: Vec<&'static str>, // non-volatile xmm an fproc preserves via `uses`
        ins: Vec<&'static str>,
        outs: Vec<&'static str>,
        reads: HashMap<&'static str, usize>, // reg → first-read line
        writes: HashSet<&'static str>,
        xmm_writes: HashMap<&'static str, usize>, // non-vol xmm → first-write line
        xmm_saved: HashSet<&'static str>,         // non-vol xmm hand-saved to memory
        framed: bool,
        rsp: Option<i64>, // offset from the post-prologue level; None = untrackable
    }
    let mut diags = Vec::new();
    let mut cur: Option<Acc> = None;
    let mut in_code = true;
    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let t = strip_comment(raw).trim();
        if t.is_empty() {
            continue;
        }
        match t.to_ascii_lowercase().as_str() {
            ".data" => in_code = false,
            ".code" | ".text" => in_code = true,
            _ => {}
        }
        if !in_code {
            continue;
        }
        let col = raw.len() - raw.trim_start().len() + 1;
        if let Some((rest, fp)) = strip_proc_kw(t) {
            let h = parse_proc(rest, fp);
            cur = Some(Acc {
                name: h.name,
                line,
                fp,
                saved: h.uses.iter().filter_map(|u| nonvol_reg(u)).collect(),
                saved_xmm: h.uses.iter().filter_map(|u| nonvol_xmm(u)).collect(),
                ins: h.ins.iter().filter_map(|u| gp_reg(u)).collect(),
                outs: h.outs.iter().filter_map(|u| gp_reg(u)).collect(),
                reads: HashMap::new(),
                writes: HashSet::new(),
                xmm_writes: HashMap::new(),
                xmm_saved: HashSet::new(),
                framed: h.frame,
                rsp: Some(0),
            });
            continue;
        }
        if t == "endproc" {
            if let Some(p) = cur.take() {
                // in: read but never set, and not declared `in`/`uses` → uninitialized.
                let mut undeclared: Vec<(&'static str, usize)> = p
                    .reads
                    .iter()
                    .filter(|(r, _)| {
                        **r != "rsp"
                            && **r != "rbp"
                            && !p.writes.contains(*r)
                            && !p.ins.contains(*r)
                            && !p.saved.contains(*r)
                    })
                    .map(|(r, l)| (*r, *l))
                    .collect();
                undeclared.sort();
                for (r, l) in undeclared {
                    diags.push(Diag {
                        line: l,
                        col: 1,
                        message: format!(
                            "proc `{}` reads `{r}` but never sets it — declare `in {r}` if it's an input",
                            p.name
                        ),
                        severity: Severity::Error,
                    });
                }
                // out: declared but never written.
                for o in &p.outs {
                    if !p.writes.contains(o) {
                        diags.push(Diag {
                            line: p.line,
                            col: 1,
                            message: format!("proc `{}` declares `out {o}` but never sets it", p.name),
                            severity: Severity::Error,
                        });
                    }
                }
                // float: a non-volatile xmm clobbered but not preserved. An `fproc`
                // saves the xmm listed in its `uses`; a hand-rolled movups save also
                // counts. Otherwise it's a lost caller value — an error in an fproc
                // (just add it to `uses`), a caution in a plain proc (switch to fproc).
                let mut xmm_lost: Vec<(&'static str, usize)> = p
                    .xmm_writes
                    .iter()
                    .filter(|(r, _)| !p.saved_xmm.contains(*r) && !p.xmm_saved.contains(*r))
                    .map(|(r, l)| (*r, *l))
                    .collect();
                xmm_lost.sort();
                for (r, l) in xmm_lost {
                    let (message, severity) = if p.fp {
                        (
                            format!("fproc `{}` modifies `{r}` (callee-saved) without saving it — add `{r}` to its `uses` so the prologue preserves it", p.name),
                            Severity::Error,
                        )
                    } else {
                        (
                            format!("proc `{}` modifies `{r}` (callee-saved on Win64) — a plain `proc` can't preserve xmm; make it an `fproc` and add `{r}` to `uses`, or save/reload it by hand", p.name),
                            Severity::Warn,
                        )
                    };
                    diags.push(Diag { line: l, col: 1, message, severity });
                }
            }
            continue;
        }
        let Some(p) = cur.as_mut() else { continue };

        // A call (invoke/comcall/obj.Method/plain call) isn't a plain instruction
        // — it clobbers the volatiles, returns in rax, and never touches a
        // callee-saved register. Don't run the instruction analyzer on it; just
        // record the rax result and, in a framed proc, check the stack is level.
        if is_any_call(t) {
            p.writes.insert("rax");
            if p.framed {
                if let Some(off) = p.rsp {
                    if off != 0 {
                        diags.push(Diag {
                            line,
                            col,
                            message: format!(
                                "rsp is off the frame level by {off} byte(s) at this call in framed proc `{}` — a stray push/sub broke the 16-byte alignment the frame guarantees",
                                p.name
                            ),
                            severity: Severity::Error,
                        });
                    }
                }
            }
            continue;
        }

        // uses: a callee-saved register modified outside the saved set.
        for w in reg_effects(t, &nonvol_reg).1 {
            if !p.saved.contains(&w) {
                diags.push(Diag {
                    line,
                    col,
                    message: format!(
                        "proc `{}` modifies `{w}` (callee-saved) without saving it — add `{w}` to its `uses` list, or the caller's value is lost",
                        p.name
                    ),
                    severity: Severity::Error,
                });
            }
        }

        // Accumulate reads/writes for the in/out checks.
        let (reads, writes) = reg_effects(t, &gp_reg);
        for r in reads {
            p.reads.entry(r).or_insert(line);
        }
        for w in &writes {
            p.writes.insert(w);
        }

        // Non-volatile xmm: track clobbers vs hand-rolled saves (a movups to memory),
        // checked against the fproc `uses` at endproc.
        if let Some(saved) = xmm_save_target(t) {
            p.xmm_saved.insert(saved);
        }
        for w in reg_effects(t, &nonvol_xmm).1 {
            p.xmm_writes.entry(w).or_insert(line);
        }

        // frame balance: track rsp across the body's own stack moves.
        if p.framed {
            let mn = t.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            let ops: Vec<&str> = t
                .split_once(char::is_whitespace)
                .map_or(Vec::new(), |(_, r)| r.split(',').map(|s| s.trim()).collect());
            if is_label_line(t) {
                p.rsp = Some(0);
            } else if mn == "push" {
                p.rsp = p.rsp.map(|o| o - 8);
            } else if mn == "pop" {
                p.rsp = p.rsp.map(|o| o + 8);
            } else if (mn == "sub" || mn == "add")
                && ops.first().copied().and_then(gp_reg) == Some("rsp")
            {
                match ops.get(1).copied().and_then(simple_imm) {
                    Some(n) => p.rsp = p.rsp.map(|o| o + if mn == "sub" { -n } else { n }),
                    None => p.rsp = None, // `sub rsp, reg` — can't track
                }
            } else if writes.contains(&"rsp") {
                p.rsp = None; // some other write to rsp — stop tracking, don't guess
            }
        }
    }
    diags
}

/// Any general register, canonicalized to its 64-bit name (eax→rax, r8d→r8).
fn gp_reg(s: &str) -> Option<&'static str> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "rax" | "eax" | "ax" | "al" | "ah" => "rax",
        "rbx" | "ebx" | "bx" | "bl" | "bh" => "rbx",
        "rcx" | "ecx" | "cx" | "cl" | "ch" => "rcx",
        "rdx" | "edx" | "dx" | "dl" | "dh" => "rdx",
        "rsi" | "esi" | "si" | "sil" => "rsi",
        "rdi" | "edi" | "di" | "dil" => "rdi",
        "rbp" | "ebp" | "bp" | "bpl" => "rbp",
        "rsp" | "esp" | "sp" | "spl" => "rsp",
        "r8" | "r8d" | "r8w" | "r8b" => "r8",
        "r9" | "r9d" | "r9w" | "r9b" => "r9",
        "r10" | "r10d" | "r10w" | "r10b" => "r10",
        "r11" | "r11d" | "r11w" | "r11b" => "r11",
        "r12" | "r12d" | "r12w" | "r12b" => "r12",
        "r13" | "r13d" | "r13w" | "r13b" => "r13",
        "r14" | "r14d" | "r14w" | "r14b" => "r14",
        "r15" | "r15d" | "r15w" | "r15b" => "r15",
        _ => return None,
    })
}

/// Registers named inside a `[ … ]` memory operand (all reads — they form the
/// address), keeping only those `canon` tracks.
fn mem_regs<F: Fn(&str) -> Option<&'static str>>(op: &str, canon: &F) -> Vec<&'static str> {
    let mut v = Vec::new();
    if let (Some(a), Some(b)) = (op.find('['), op.rfind(']')) {
        for tok in op[a + 1..b].split(|c: char| !c.is_alphanumeric() && c != '_') {
            if let Some(r) = canon(tok) {
                v.push(r);
            }
        }
    }
    v
}

/// A bare register operand (not a `[mem]`, not an immediate), as a tracked reg.
fn bare_reg<F: Fn(&str) -> Option<&'static str>>(op: &str, canon: &F) -> Option<&'static str> {
    let o = op.trim();
    if o.contains('[') {
        return None;
    }
    canon(o)
}

/// `obj.Method( … )` shape — a typed COM call (also a call site).
fn is_method_call_shape(t: &str) -> bool {
    t.split_once('.').is_some_and(|(obj, rest)| {
        let obj_ok = !obj.is_empty() && obj.chars().all(|c| c.is_alphanumeric() || c == '_');
        let m_ok = rest
            .split_once('(')
            .is_some_and(|(m, _)| !m.trim().is_empty() && m.trim().chars().all(|c| c.is_alphanumeric() || c == '_'));
        obj_ok && m_ok
    })
}

/// True if `t` is an `invoke`/`comcall`/`obj.Method(…)` — a call to *external*
/// code (Windows API / COM) that strictly follows the Win64 ABI and so destroys
/// the caller-saved registers. A plain `call <label>` is deliberately excluded:
/// it targets one of your own functions, which may preserve more than the ABi
/// requires (e.g. a helper that keeps the loop counters), so assuming a clobber
/// there would be a false positive.
fn is_call_site(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.starts_with("invoke ")
        || l.starts_with("invoke\t")
        || l.starts_with("comcall ")
        || is_method_call_shape(t)
}

/// The registers (that `canon` tracks) an instruction reads and writes —
/// conservative: unknown mnemonics treat operand 0 as written so a clobber is
/// cleared, never invented.
fn reg_effects<F: Fn(&str) -> Option<&'static str>>(
    t: &str,
    canon: &F,
) -> (Vec<&'static str>, Vec<&'static str>) {
    let (mn, rest) = match t.find(char::is_whitespace) {
        Some(p) => (t[..p].to_ascii_lowercase(), t[p..].trim()),
        None => (t.to_ascii_lowercase(), ""),
    };
    let ops: Vec<String> = if rest.is_empty() { Vec::new() } else { split_top_commas(rest) };
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    let rd = |r: &str| canon(r);
    // Implicit rdx:rax operands.
    match mn.as_str() {
        "cdq" | "cqo" | "cwd" => {
            if let Some(r) = rd("rdx") { writes.push(r); }
            if let Some(r) = rd("rax") { reads.push(r); }
        }
        "mul" => {
            if let Some(r) = rd("rax") { reads.push(r); writes.push(r); }
            if let Some(r) = rd("rdx") { writes.push(r); }
        }
        "div" | "idiv" => {
            if let Some(r) = rd("rax") { reads.push(r); writes.push(r); }
            if let Some(r) = rd("rdx") { reads.push(r); writes.push(r); }
        }
        _ => {}
    }
    for op in &ops {
        reads.extend(mem_regs(op, canon)); // address registers are always reads
    }
    let pure_write = matches!(
        mn.as_str(),
        "mov" | "lea" | "movzx" | "movsx" | "movsxd" | "movabs" | "pop" | "cvtsi2ss" | "cvtsi2sd"
            | "cvttss2si" | "cvttsd2si" | "cvtss2si" | "cvtsd2si"
    ) || mn.starts_with("set");
    let read_modify = matches!(
        mn.as_str(),
        "add" | "sub" | "and" | "or" | "xor" | "adc" | "sbb" | "shl" | "shr" | "sar" | "sal"
            | "rol" | "ror" | "rcl" | "rcr" | "inc" | "dec" | "neg" | "not" | "imul" | "xchg"
    );
    let read_only_first = matches!(mn.as_str(), "cmp" | "test" | "push" | "mul" | "div" | "idiv");
    let branch = mn.starts_with('j') || mn == "call"; // op0 is a target → a read
    let zeroing = matches!(mn.as_str(), "xor" | "sub")
        && ops.len() == 2
        && bare_reg(&ops[0], canon).is_some()
        && bare_reg(&ops[0], canon) == bare_reg(&ops[1], canon);
    if let Some(r0) = ops.first().and_then(|o| bare_reg(o, canon)) {
        if branch || read_only_first {
            reads.push(r0);
        } else if zeroing || pure_write {
            writes.push(r0);
        } else if read_modify {
            writes.push(r0);
            reads.push(r0);
        } else {
            writes.push(r0); // unknown: assume it defines op0 (avoids false positives)
        }
    }
    if !zeroing {
        for op in ops.iter().skip(1) {
            if let Some(r) = bare_reg(op, canon) {
                reads.push(r);
            }
        }
    }
    (reads, writes)
}

/// Warn when a caller-saved register is read after an `invoke`/`call`/COM call
/// destroyed its value — the classic Win64 bug of stashing a pointer in
/// rcx/rdx/r8/r9/r10/r11, calling something, then using the now-garbage register.
///
/// Conservative by construction: it tracks only those six registers, resets at
/// every label (so it never reasons across a branch it can't see), treats rax as
/// the return value, and on a call checks the argument registers *before*
/// clobbering. The aim is zero false positives on correct code.
pub fn clobber_diags(src: &str) -> Vec<Diag> {
    let pos = |r: &str| CLOBBER_REGS.iter().position(|&x| x == r).unwrap();
    let mut clobbered: [Option<usize>; 6] = [None; 6]; // Some(line of the call)
    let mut diags = Vec::new();
    let mut in_code = true;
    let mut in_macro = false;
    let warn = |diags: &mut Vec<Diag>, line: usize, reg: &str, call: usize| {
        diags.push(Diag {
            line,
            col: 1,
            message: format!(
                "`{reg}` may be clobbered by the call at line {call} — reload it before using it here (it's caller-saved)"
            ),
            severity: Severity::Warn,
        });
    };
    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let body = strip_comment(raw);
        let mut t = body.trim();
        if t.is_empty() {
            continue;
        }
        match t.to_ascii_lowercase().as_str() {
            ".data" => {
                in_code = false;
                continue;
            }
            ".code" | ".text" => {
                in_code = true;
                continue;
            }
            _ => {}
        }
        if !in_code {
            continue;
        }
        if parse_macro_def(t).is_some() {
            in_macro = true;
            continue;
        }
        if is_endm(t) {
            in_macro = false;
            continue;
        }
        if in_macro {
            continue;
        }
        // A `proc`/`endproc` boundary starts a fresh function: its registers come
        // from the caller (its `in` args are live, the rest indeterminate), not
        // from whatever code physically precedes it. Reset — else a preceding
        // proc's trailing `invoke` would falsely taint this proc's argument reads.
        let head0 = t.split_whitespace().next().unwrap_or("");
        if head0.eq_ignore_ascii_case("proc") || head0.eq_ignore_ascii_case("endproc") {
            clobbered = [None; 6];
            continue;
        }
        // Peel a leading `label:`; a bare label (or any label) starts a new block
        // whose incoming register state we can't know — reset.
        if let Some((head, tail)) = t.split_once(':') {
            if !head.is_empty()
                && head.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$')
                && !head.contains(char::is_whitespace)
            {
                clobbered = [None; 6];
                t = tail.trim();
                if t.is_empty() {
                    continue;
                }
            }
        }
        if is_call_site(t) {
            for r in reg_effects(t, &clobber_reg).0 {
                if let Some(c) = clobbered[pos(r)] {
                    warn(&mut diags, line, r, c);
                }
            }
            // every caller-saved register is now destroyed
            clobbered = [Some(line); 6];
            continue;
        }
        let mn = t.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let (reads, writes) = reg_effects(t, &clobber_reg);
        for r in reads {
            if let Some(c) = clobbered[pos(r)] {
                warn(&mut diags, line, r, c);
            }
        }
        for r in writes {
            clobbered[pos(r)] = None;
        }
        // After an unconditional transfer the fall-through is a new block.
        if mn == "ret" || mn == "jmp" {
            clobbered = [None; 6];
        }
    }
    diags
}

/// Check `src` and return diagnostics — semantic (invoke arg count, unknown
/// constants with a "did you mean", bad struct fields) plus a whole-file
/// syntax/encode pass through rasm. Empty result = clean.
pub fn check(src: &str, kb: &Kb) -> Vec<Diag> {
    let mut labels = collect_labels(src);
    labels.extend(macro_names(src)); // a macro invocation is not an unknown name
    let com_objs = collect_com_objs(src);
    labels.extend(com_objs.keys().cloned());
    // Equates are their own namespace and win over winkb constants (eval_expr checks
    // them first), so register the names: a defined equate is a known identifier, not
    // an unknown constant. Where a name also matches a Windows constant, note the
    // (intentional) shadow once at its definition rather than erroring on every use.
    let eqdefs = collect_equate_defs(src);
    for (name, _) in &eqdefs {
        labels.insert(name.clone());
    }
    let mut diags = Vec::new();
    for (name, line) in &eqdefs {
        let shadowed = match kb.resolve(name) {
            Ok(v) if !v.is_empty() => Some(name.clone()),
            _ => kb
                .suggest(name, 1)
                .ok()
                .and_then(|s| s.into_iter().find(|c| c.eq_ignore_ascii_case(name))),
        };
        if let Some(c) = shadowed {
            diags.push(Diag {
                line: *line,
                col: 1,
                message: format!(
                    "equate '{name}' shadows the Windows constant '{c}' — the equate value is used"
                ),
                severity: Severity::Warn,
            });
        }
    }
    let mut in_macro = false;
    let mut string_block_end: Option<&str> = None;
    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let body = strip_comment(raw);
        let t = body.trim();
        // Raw string block: its interior is arbitrary text, not code — skip it.
        if let Some(end_kw) = string_block_end {
            if raw.trim().eq_ignore_ascii_case(end_kw) {
                string_block_end = None;
            }
            continue;
        }
        if t.eq_ignore_ascii_case(".asciistring") {
            string_block_end = Some(".endasciistring");
            continue;
        }
        if t.eq_ignore_ascii_case(".widestring") {
            string_block_end = Some(".endwidestring");
            continue;
        }
        // `.include` is expanded before checking; ignore an unexpanded one.
        if include_path(raw).is_some() {
            continue;
        }
        if t.is_empty() {
            continue;
        }
        // A macro definition's params/body are macro-local — skip them here; real
        // errors surface when the expanded code is assembled below.
        if parse_macro_def(t).is_some() {
            in_macro = true;
            continue;
        }
        if is_endm(t) {
            in_macro = false;
            continue;
        }
        if in_macro {
            continue;
        }
        // `proc`/`endproc` are handled by their own contract pass below; skip the
        // ident scan so the register lists aren't read as unknown constants.
        if strip_keyword(t, "proc").is_some() || t == "endproc" {
            continue;
        }
        // invoke arg-count check
        if let Some(rest) = strip_keyword(t, "invoke") {
            let parts = split_top_commas(rest);
            let func = parts.first().map(|s| s.trim()).unwrap_or("");
            let nargs = if func.is_empty() { 0 } else { parts.len() - 1 };
            if let Ok(Some(f)) = kb.function(func) {
                if f.params.len() != nargs {
                    let col = body.find(func).map(|c| c + 1).unwrap_or(1);
                    diags.push(Diag {
                        line,
                        col,
                        message: format!("{func} takes {} argument(s), got {nargs}", f.params.len()),
                        severity: Severity::Error,
                    });
                }
            }
        }
        // comcall: the interface and method must exist (db-aware COM call).
        if let Some(rest) = strip_keyword(t, "comcall") {
            let parts = split_top_commas(rest);
            if parts.len() >= 3 {
                let iface = parts[1].trim();
                let method = parts[2].trim();
                match kb.interface(iface) {
                    Ok(None) => {
                        let col = body.find(iface).map(|c| c + 1).unwrap_or(1);
                        diags.push(Diag { line, col, message: format!("unknown interface '{iface}'"), severity: Severity::Error });
                    }
                    Ok(Some(_)) if matches!(vtable_index_of(kb, iface, method), Ok(None)) => {
                        let col = body.rfind(method).map(|c| c + 1).unwrap_or(1);
                        diags.push(Diag {
                            line,
                            col,
                            message: format!("{iface} has no method '{method}'"),
                            severity: Severity::Error,
                        });
                    }
                    _ => {}
                }
            }
            continue; // names here are interfaces/methods, not constants
        }
        // iid: the interface must exist.
        if let Some(rest) = strip_keyword(t, "iid") {
            let iface = rest.trim();
            if matches!(kb.interface(iface), Ok(None)) {
                let col = body.find(iface).map(|c| c + 1).unwrap_or(1);
                diags.push(Diag { line, col, message: format!("unknown interface '{iface}'"), severity: Severity::Error });
            }
            continue;
        }
        // comobj NAME : Interface — the interface must exist.
        if let Some(rest) = strip_keyword(t, "comobj") {
            if let Some((_, iface)) = rest.split_once(':') {
                let iface = iface.trim();
                if matches!(kb.interface(iface), Ok(None)) {
                    let col = body.find(iface).map(|c| c + 1).unwrap_or(1);
                    diags.push(Diag { line, col, message: format!("unknown interface '{iface}'"), severity: Severity::Error });
                }
            }
            continue;
        }
        // obj.Method(args) — the method must exist on the bound interface.
        if let Some((_, iface, method, _)) = parse_method_call(t, &com_objs) {
            if matches!(vtable_index_of(kb, &iface, &method), Ok(None)) {
                let col = body.rfind(&method).map(|c| c + 1).unwrap_or(1);
                diags.push(Diag { line, col, message: format!("{iface} has no method '{method}'"), severity: Severity::Error });
            }
            continue;
        }
        // MASM data: validate each field value fits its size, then skip the ident
        // scan (the type keyword would otherwise look like an unknown constant).
        if let Some((_, type_kw, values)) = parse_data_line(t) {
            if let Some((_, width, _, _)) = data_type(type_kw) {
                for val in split_top_commas(values) {
                    let v = val.trim();
                    if let Some(n) = data_value_i64(v) {
                        if !fits_width(n, width) {
                            let col = body.find(v).map(|c| c + 1).unwrap_or(1);
                            diags.push(Diag {
                                line,
                                col,
                                message: format!("{n} doesn't fit a {width}-byte {type_kw} field"),
                                severity: Severity::Error,
                            });
                        }
                    }
                }
            }
            continue;
        }
        if !t.starts_with('.') {
            check_idents(body, line, kb, &labels, &mut diags);
        }
    }
    // Whole-file syntax/encode pass, with the failing line recovered. A lowering
    // error already carries its *source* line; a `rasm::assemble` error carries a
    // *lowered* line, mapped back through `map`. Column points at the first
    // non-blank char so the squiggle hugs the offending token, not the indent.
    let mk = |line: usize, message: String| -> Diag {
        let col = match line {
            0 => 0,
            n => src
                .lines()
                .nth(n - 1)
                .map(|l| l.len() - l.trim_start().len() + 1)
                .unwrap_or(1),
        };
        Diag { line, col, message, severity: Severity::Error }
    };
    match lower_mapped(src, kb) {
        Err(e) => {
            let (line, msg) = split_line_tag(&format!("{e:#}"));
            diags.push(mk(line.unwrap_or(0), msg));
        }
        Ok((low, map)) => {
            if let Err(e) = rasm::assemble(&low) {
                let (lowered, msg) = split_line_tag(&format!("{e:#}"));
                let line = lowered
                    .and_then(|n| map.get(n.saturating_sub(1)).copied())
                    .unwrap_or(0);
                diags.push(mk(line, msg));
            }
        }
    }
    // Caller-saved register clobbered across a call — a hint, appended last.
    diags.extend(clobber_diags(src));
    // Callee side: a `proc` modifying a callee-saved register it didn't declare.
    diags.extend(proc_contract_diags(src));
    diags
}

/// Split a leading ``line N: `` tag off an error message, returning `N` and the
/// remainder. Both [`lower_mapped`] and `rasm::assemble` prefix a failure this
/// way, so peeling it lets the caller place the line itself (in [`Diag::line`])
/// without the message contradicting it with a stale/lowered number.
fn split_line_tag(msg: &str) -> (Option<usize>, String) {
    if let Some(after) = msg.strip_prefix("line ") {
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Some(rest) = after[digits.len()..].strip_prefix(": ") {
                return (digits.parse().ok(), rest.to_string());
            }
        }
    }
    (None, msg.to_string())
}

/// Flag `Struct.field` typos and unknown constant-like identifiers.
fn check_idents(body: &str, line: usize, kb: &Kb, labels: &HashSet<String>, diags: &mut Vec<Diag>) {
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c == '\'' || c == '"' {
            i += 1;
            while i < b.len() && b[i] as char != c {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (is_ident_char(b[i] as char) || b[i] as char == '.') {
                i += 1;
            }
            let tok = &body[start..i];
            let col = start + 1;
            if let Some(dot) = tok.find('.') {
                let (lhs, field) = (&tok[..dot], &tok[dot + 1..]);
                if !is_register(lhs) && !labels.contains(lhs) {
                    if let Ok(Some(layout)) = kb.layout(lhs) {
                        // Validate the (possibly nested) path; suggest against the head.
                        if !field.is_empty() && matches!(field_path(kb, lhs, field), Ok(None)) {
                            let head = field.split('.').next().unwrap_or(field);
                            let near = layout
                                .fields
                                .iter()
                                .min_by_key(|f| lev(head, &f.name))
                                .map(|f| format!(" — did you mean '{}.{}'?", lhs, f.name))
                                .unwrap_or_default();
                            diags.push(Diag {
                                line,
                                col,
                                message: format!("{lhs} has no field '{field}'{near}"),
                                severity: Severity::Error,
                            });
                        }
                    }
                }
            } else if is_constant_like(tok) && !labels.contains(tok) {
                if matches!(kb.resolve(tok), Ok(v) if v.is_empty()) {
                    if let Ok(s) = kb.suggest(tok, 1) {
                        if let Some(best) = s.first() {
                            diags.push(Diag {
                                line,
                                col,
                                message: format!("unknown constant '{tok}' — did you mean '{best}'?"),
                                severity: Severity::Error,
                            });
                        }
                    }
                }
            }
            continue;
        }
        i += 1;
    }
}

/// Looks like a Windows constant: UPPER_SNAKE or all-caps, with a letter.
fn is_constant_like(s: &str) -> bool {
    s.len() > 2
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn lev(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ac) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &bc) in b.iter().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// A collected MASM-style macro: its parameters, `LOCAL` label names, and raw
/// body lines.
struct Macro {
    params: Vec<String>,
    locals: Vec<String>,
    body: Vec<String>,
}

/// Recognize a MASM macro header `NAME MACRO [p1, p2, …]` → `(name, params)`. The
/// `MACRO` keyword is case-insensitive; the name is a normal (case-sensitive)
/// symbol. `None` if the line isn't a macro header.
fn parse_macro_def(t: &str) -> Option<(String, Vec<String>)> {
    let (name, rest) = t.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    if rest.len() < 5 || !rest[..5].eq_ignore_ascii_case("macro") {
        return None;
    }
    let params_str = &rest[5..];
    if !(params_str.is_empty() || params_str.starts_with(char::is_whitespace)) {
        return None; // e.g. `MACROX`
    }
    if name.is_empty() || !name.chars().all(is_ident_char) {
        return None;
    }
    let params = if params_str.trim().is_empty() {
        Vec::new()
    } else {
        split_top_commas(params_str).iter().map(|s| s.trim().to_string()).collect()
    };
    Some((name.to_string(), params))
}

/// `ENDM` (case-insensitive) — the macro terminator.
fn is_endm(t: &str) -> bool {
    t.eq_ignore_ascii_case("endm")
}

/// A `LOCAL a, b, …` declaration's names (case-insensitive keyword), if any.
fn parse_local(t: &str) -> Option<Vec<String>> {
    let (kw, rest) = t.split_once(char::is_whitespace)?;
    if !kw.eq_ignore_ascii_case("local") {
        return None;
    }
    Some(
        split_top_commas(rest)
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// Expand MASM-style macros (`NAME MACRO` / `LOCAL` / `ENDM`) and their
/// invocations, returning the macro-free source plus a map from each output line
/// (0-based) to the 1-based original line (a body maps to the invocation line).
/// Definitions produce no output — they generate no code.
fn expand_macros(src: &str) -> Result<(String, Vec<usize>)> {
    // Pass 1: collect definitions; keep the rest with their original line numbers.
    let mut macros: HashMap<String, Macro> = HashMap::new();
    let mut kept: Vec<(String, usize)> = Vec::new();
    let mut collecting: Option<(String, Macro)> = None;
    for (i, raw) in src.lines().enumerate() {
        let t = strip_comment(raw).trim();
        if let Some((_, mac)) = collecting.as_mut() {
            if is_endm(t) {
                let (name, mac) = collecting.take().unwrap();
                macros.insert(name, mac);
            } else if let Some(mut ls) = parse_local(t) {
                mac.locals.append(&mut ls);
            } else if parse_macro_def(t).is_some() {
                bail!("line {}: nested macro definition is not supported", i + 1);
            } else {
                mac.body.push(raw.to_string());
            }
            continue;
        }
        if let Some((name, params)) = parse_macro_def(t) {
            collecting = Some((name, Macro { params, locals: Vec::new(), body: Vec::new() }));
            continue;
        }
        kept.push((raw.to_string(), i + 1));
    }
    if collecting.is_some() {
        bail!("macro definition without `ENDM`");
    }
    // Pass 2: expand invocations (recursively, so a macro may use another). Each
    // invocation gets a fresh id so its `LOCAL` labels are unique.
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut exp_ctr = 0usize;
    for (line, orig) in kept {
        expand_line(&line, orig, &macros, &mut out, 0, &mut exp_ctr)?;
    }
    let mut esrc = String::new();
    let mut emap = Vec::with_capacity(out.len());
    for (line, orig) in out {
        esrc.push_str(&line);
        esrc.push('\n');
        emap.push(orig);
    }
    Ok((esrc, emap))
}

/// Expand `line` (from original line `orig`) into `out`: if its first word names
/// a macro, substitute the arguments + `LOCAL`s and recurse on the body; else
/// pass it through.
fn expand_line(
    line: &str,
    orig: usize,
    macros: &HashMap<String, Macro>,
    out: &mut Vec<(String, usize)>,
    depth: usize,
    exp_ctr: &mut usize,
) -> Result<()> {
    if depth > 64 {
        bail!("macro expansion too deep (recursive macro?)");
    }
    let body = strip_comment(line).trim();
    let name = body.split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
    let Some(mac) = macros.get(name) else {
        out.push((line.to_string(), orig));
        return Ok(());
    };
    let args_str = body[name.len()..].trim();
    let args: Vec<String> = if args_str.is_empty() {
        Vec::new()
    } else {
        split_top_commas(args_str).iter().map(|s| s.trim().to_string()).collect()
    };
    if args.len() != mac.params.len() {
        bail!("macro `{name}` expects {} argument(s), got {}", mac.params.len(), args.len());
    }
    let eid = *exp_ctr;
    *exp_ctr += 1;
    // Substitutions: params → args, then `LOCAL`s → per-expansion unique labels.
    let mut subs: Vec<(&str, String)> = mac.params.iter().map(String::as_str).zip(args).collect();
    for l in &mac.locals {
        subs.push((l.as_str(), format!("{l}__m{eid}")));
    }
    for bline in &mac.body {
        let sub = substitute(bline, &subs);
        expand_line(&sub, orig, macros, out, depth + 1, exp_ctr)?;
    }
    Ok(())
}

/// Whole-word substitution of `(token → replacement)` pairs in one line;
/// string/char literals are copied verbatim.
fn substitute(line: &str, subs: &[(&str, String)]) -> String {
    let b = line.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c == '\'' || c == '"' {
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i] as char);
                let done = b[i] as char == c;
                i += 1;
                if done {
                    break;
                }
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let tok = &line[start..i];
            match subs.iter().find(|(from, _)| *from == tok) {
                Some((_, to)) => out.push_str(to),
                None => out.push_str(tok),
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The names of every macro defined in `src`.
fn macro_names(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|raw| parse_macro_def(strip_comment(raw).trim()).map(|(name, _)| name))
        .collect()
}

// ── data declarations (MASM-style) ────────────────────────────────────────────

/// Map a MASM data-type keyword to `(encoder directive, width bytes, wide)`.
/// `WCHAR` is a 2-byte field whose strings encode as UTF-16LE. Case-insensitive;
/// `S`-prefixed (signed) and `Dx` aliases included.
// Returns (gas directive, byte width, wide-string?, float?). `float` data takes a
// decimal literal (`real8 440.0`) emitted as its IEEE-754 bit pattern.
fn data_type(kw: &str) -> Option<(&'static str, usize, bool, bool)> {
    Some(match kw.to_ascii_uppercase().as_str() {
        "BYTE" | "SBYTE" | "DB" => ("byte", 1, false, false),
        "WORD" | "SWORD" | "DW" => ("word", 2, false, false),
        "WCHAR" => ("word", 2, true, false),
        "DWORD" | "SDWORD" | "DD" => ("long", 4, false, false),
        "QWORD" | "SQWORD" | "DQ" => ("quad", 8, false, false),
        "REAL4" | "F32" => ("long", 4, false, true),
        "REAL8" | "F64" => ("quad", 8, false, true),
        _ => return None,
    })
}

/// A float data literal → its IEEE-754 bits as a hex string (`.long`/`.quad` operand).
/// A `0x…` literal is taken verbatim as the raw bit pattern.
fn float_bits(v: &str, width: usize) -> Result<String> {
    let v = v.trim();
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        let bits = u64::from_str_radix(hex, 16).with_context(|| format!("bad float bits '{v}'"))?;
        return Ok(format!("0x{bits:X}"));
    }
    if width == 4 {
        let f: f32 = v.parse().with_context(|| format!("bad float literal '{v}'"))?;
        Ok(format!("0x{:X}", f.to_bits()))
    } else {
        let f: f64 = v.parse().with_context(|| format!("bad float literal '{v}'"))?;
        Ok(format!("0x{:X}", f.to_bits()))
    }
}

/// Split off the first whitespace-delimited word; the rest is left-trimmed.
fn split_first(s: &str) -> (&str, &str) {
    match s.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim_start()),
        None => (s, ""),
    }
}

/// `[ … ]` or `PTR …` — a size-prefixed memory operand, not a data value.
fn value_is_operand(v: &str) -> bool {
    let v = v.trim_start();
    v.starts_with('[') || v.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("ptr"))
}

/// Detect a data definition `[label] TYPE value, …`. The value part must not
/// begin with `[` or `PTR`, so an instruction's size prefix (`mov BYTE PTR
/// [rax], 1`) is never mistaken for data. Returns `(label, type_kw, values)`.
fn parse_data_line(t: &str) -> Option<(Option<&str>, &str, &str)> {
    let (w0, rest0) = split_first(t);
    if data_type(w0).is_some() && !rest0.is_empty() && !value_is_operand(rest0) {
        return Some((None, w0, rest0));
    }
    let (w1, rest1) = split_first(rest0);
    if data_type(w1).is_some()
        && !w0.is_empty()
        && w0.chars().all(is_ident_char)
        && !rest1.is_empty()
        && !value_is_operand(rest1)
    {
        return Some((Some(w0), w1, rest1));
    }
    None
}

/// `N dup(value)` → `(count, value)`.
fn parse_dup(v: &str) -> Option<(usize, &str)> {
    let pos = v.to_ascii_lowercase().find("dup(")?;
    let count: usize = v[..pos].trim().parse().ok()?;
    let inner = &v[pos + 4..];
    let close = inner.rfind(')')?;
    Some((count, inner[..close].trim()))
}

/// The content of a `"…"` string with basic escapes processed (for wide strings;
/// narrow strings go through the encoder's `.ascii`, which handles its own).
fn string_content(quoted: &str) -> String {
    let inner = quoted.trim().trim_start_matches('"').trim_end_matches('"');
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Lower a MASM data definition to encoder directives under its label. Strings
/// in a `BYTE` field become `.ascii`; in a `WCHAR` field, UTF-16LE `.word`s.
fn lower_data(
    label: Option<&str>,
    type_kw: &str,
    values: &str,
    kb: &Kb,
    labels: &HashSet<String>,
) -> Result<String> {
    let (dir, width, wide, is_float) = data_type(type_kw).expect("caller checked the type");
    let mut out = String::new();
    if let Some(l) = label {
        out.push_str(&format!("{l}:\n"));
    }
    for val in split_top_commas(values) {
        let v = val.trim();
        if v.is_empty() {
            continue;
        }
        if v == "?" {
            out.push_str(&format!("  .zero {width}\n"));
        } else if is_float {
            // `real4`/`real8` value: a decimal float (or 0x bit pattern) → IEEE bits.
            if let Some((count, inner)) = parse_dup(v) {
                let bits = float_bits(inner, width)?;
                for _ in 0..count {
                    out.push_str(&format!("  .{dir} {bits}\n"));
                }
            } else {
                out.push_str(&format!("  .{dir} {}\n", float_bits(v, width)?));
            }
        } else if v.starts_with('"') {
            if wide {
                let units: Vec<String> =
                    string_content(v).encode_utf16().map(|u| u.to_string()).collect();
                if !units.is_empty() {
                    out.push_str(&format!("  .word {}\n", units.join(", ")));
                }
            } else if width == 1 {
                out.push_str(&format!("  .ascii {v}\n"));
            } else {
                bail!("a string needs a BYTE or WCHAR field, not {type_kw}");
            }
        } else if let Some((count, inner)) = parse_dup(v) {
            if inner == "0" {
                out.push_str(&format!("  .zero {}\n", count * width));
            } else {
                let r = resolve_operands(inner, kb, labels)?;
                for _ in 0..count {
                    out.push_str(&format!("  .{dir} {r}\n"));
                }
            }
        } else {
            let r = resolve_operands(v, kb, labels)?;
            out.push_str(&format!("  .{dir} {r}\n"));
        }
    }
    Ok(out)
}

/// Parse a literal data value to `i64` for range-checking; `None` for a string,
/// constant, or expression (assumed to fit).
fn data_value_i64(v: &str) -> Option<i64> {
    let v = v.trim();
    if v.starts_with('\'') && v.ends_with('\'') && v.chars().count() >= 3 {
        return v[1..].chars().next().map(|c| c as i64);
    }
    let (neg, body) = v.strip_prefix('-').map_or((false, v), |b| (true, b));
    let body = body.replace('_', "");
    let n = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()?
    } else if !body.is_empty() && body.chars().all(|c| c.is_ascii_digit()) {
        body.parse().ok()?
    } else {
        return None;
    };
    Some(if neg { -n } else { n })
}

/// Does `v` fit in a `width`-byte field, as either a signed or unsigned value?
fn fits_width(v: i64, width: usize) -> bool {
    if width >= 8 {
        return true;
    }
    let bits = (width * 8) as u32;
    let v = v as i128;
    v >= -(1i128 << (bits - 1)) && v < (1i128 << bits)
}

/// Lower `src` to rasm-ready Intel-syntax text.
/// Expand `.include "file"` directives, splicing each referenced file in place
/// (path relative to the including file's directory), recursively — so a large
/// program can be composed from a font, a palette, a primitives library, etc.
/// Call this on the raw text (with the main file's path) before `lower`/`check`.
/// The result of textual include expansion, plus the structure that flattening
/// otherwise throws away: which file each output line came from, and the
/// parent→child `.include` edges. `.include` stays purely textual — `text` is
/// byte-for-byte what the old expander produced — this merely *records* the
/// shell-of-includes graph so later passes (and `--include-graph`) can see it.
pub struct Expansion {
    /// The fully expanded source.
    pub text: String,
    /// Interned file paths; the index is the file id.
    pub files: Vec<std::path::PathBuf>,
    /// For each 0-based line of `text`, the id of the file that line came from.
    pub line_file: Vec<u32>,
    /// For each 0-based line of `text`, its 1-based line number within its own
    /// file — so a diagnostic on the expanded text maps back to the editor.
    pub line_no: Vec<u32>,
    /// One per `.include`: `(parent file id, child file id, 1-based line in parent)`.
    pub edges: Vec<(u32, u32, u32)>,
}

impl Expansion {
    /// File id for `p`, interning it on first sight (a file included from several
    /// places is one node with several edges — a DAG, not a tree).
    fn intern(&mut self, p: &std::path::Path) -> u32 {
        if let Some(i) = self.files.iter().position(|f| f == p) {
            return i as u32;
        }
        self.files.push(p.to_path_buf());
        (self.files.len() - 1) as u32
    }
}

pub fn expand_includes(src: &str, from: &std::path::Path) -> Result<String> {
    Ok(expand_includes_graph(src, from)?.text)
}

/// Like [`expand_includes`], but also returns the include graph + per-line file map.
pub fn expand_includes_graph(src: &str, from: &std::path::Path) -> Result<Expansion> {
    let mut e = Expansion {
        text: String::new(),
        files: Vec::new(),
        line_file: Vec::new(),
        line_no: Vec::new(),
        edges: Vec::new(),
    };
    let root = e.intern(from);
    let dir = from.parent().unwrap_or_else(|| std::path::Path::new("."));
    expand_includes_rec(src, dir, 0, root, &mut e)?;
    Ok(e)
}

fn include_path(line: &str) -> Option<&str> {
    let r = strip_keyword(strip_comment(line).trim(), ".include")?.trim();
    r.strip_prefix('"').and_then(|x| x.strip_suffix('"'))
}

fn expand_includes_rec(
    src: &str,
    dir: &std::path::Path,
    depth: usize,
    file: u32,
    e: &mut Expansion,
) -> Result<()> {
    if depth > 32 {
        bail!("`.include` nested too deeply (a cycle?)");
    }
    for (idx, line) in src.lines().enumerate() {
        if let Some(path) = include_path(line) {
            let full = dir.join(path);
            let child = e.intern(&full);
            e.edges.push((file, child, (idx + 1) as u32));
            let content = std::fs::read_to_string(&full)
                .with_context(|| format!("`.include \"{path}\"`: cannot read {}", full.display()))?;
            let sub = full.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.to_path_buf());
            expand_includes_rec(&content, &sub, depth + 1, child, e)?;
        } else {
            e.text.push_str(line);
            e.text.push('\n');
            e.line_file.push(file);
            e.line_no.push((idx + 1) as u32);
        }
    }
    Ok(())
}

/// Module-scoped labels (the `--modules` gate), using the include graph's per-line
/// file attribution. A module is `module NAME … endmodule` markers WITHIN A FILE,
/// and a region scopes only the lines OF THAT FILE — content pulled in by `.include`
/// is NOT absorbed (it carries its own file's modules, or none), even though it sits
/// textually inside the region. A file may hold several module regions; a module may
/// be declared by many files, and their private labels pool by NAME.
///
/// A label whose name starts with a capital is EXPORTED (global, still unique); a
/// lowercase/`_` name is PRIVATE — mangled `NAME$label` so two modules may reuse a
/// helper/data name (`loop`, `fb`, `pu_loop`). `.globl`/`.global` pins a name global.
/// This narrows the old "every label is global" rule; code outside any module is
/// untouched. Only label *names* change, never bytes (same offsets); line count is
/// preserved; the mangled names show in `--emit-asm`. `module`/`endmodule` are no-ops
/// in the lowering pass.
///
/// Note the include graph is only the *mechanism* (it tells us which file a line came
/// from); the module layout is *defined* by the markers, independent of how files are
/// included.
pub fn scope_modules_by_file(exp: &Expansion) -> String {
    scope_modules(&exp.text, &exp.line_file)
}

/// The core of [`scope_modules_by_file`], split out so it is unit-testable with a
/// hand-built attribution. `line_file[i]` is the file id of expanded line `i`.
///
/// Two nested private tiers, both ONLY inside a module (outside one, every label is
/// global, exactly as before):
/// * a label DEFINED inside a `proc … endproc` is private to that **proc**
///   (`proc$label`) — so two procs in one module may reuse a jump-target name
///   (`loop`, `done`) you couldn't otherwise see across the module's many files;
/// * a lowercase label at **module level** (outside any proc) is private to the
///   module (`Module$label`).
/// Capitalised and `.globl`'d names stay exported at either level. The proc tier is
/// strictly finer, so it can only remove collisions (a proc's internals are only
/// referenced within that proc — nobody jumps into the middle of one).
fn scope_modules(text: &str, line_file: &[u32]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != line_file.len() {
        return text.to_string(); // attribution mismatch — be safe, do nothing
    }

    // Per line: the module and the proc it sits in, each tracked per file id so an
    // `.include`d file keeps its own module/proc instead of being absorbed. A proc is
    // an inner scope only inside a module. `.ASCIISTRING`/`.WIDESTRING` raw blocks pass
    // through untouched (no module/proc, never renamed).
    let mut open_mod: HashMap<u32, &str> = HashMap::new();
    let mut open_proc: HashMap<u32, &str> = HashMap::new();
    let mut line_mod: Vec<Option<&str>> = vec![None; lines.len()];
    let mut line_proc: Vec<Option<&str>> = vec![None; lines.len()];
    let mut proc_module: HashMap<&str, &str> = HashMap::new(); // proc name -> its module
    let mut raw_end: Option<&'static str> = None;
    for (i, raw) in lines.iter().enumerate() {
        if let Some(end) = raw_end {
            if raw.trim().eq_ignore_ascii_case(end) {
                raw_end = None;
            }
            continue;
        }
        let f = line_file[i];
        let t = strip_comment(raw).trim();
        if t.eq_ignore_ascii_case(".asciistring") {
            raw_end = Some(".endasciistring");
        } else if t.eq_ignore_ascii_case(".widestring") {
            raw_end = Some(".endwidestring");
        } else if let Some(rest) = strip_keyword(t, "module") {
            open_mod.insert(f, rest.split_whitespace().next().unwrap_or(""));
        } else if t == "endmodule" {
            open_mod.remove(&f);
        } else if let Some(rest) = strip_keyword(t, "proc") {
            // The proc HEADER is module-level; its body (following lines) is the inner
            // scope — but only when the proc sits inside a module.
            let m = open_mod.get(&f).copied().filter(|m| !m.is_empty());
            line_mod[i] = m;
            if let (Some(m), Some(p)) = (m, rest.split_whitespace().next()) {
                if !p.is_empty() {
                    open_proc.insert(f, p);
                    proc_module.entry(p).or_insert(m);
                }
            }
        } else if t == "endproc" {
            open_proc.remove(&f);
            line_mod[i] = open_mod.get(&f).copied().filter(|m| !m.is_empty());
        } else if let Some(&m) = open_mod.get(&f) {
            if !m.is_empty() {
                line_mod[i] = Some(m);
                line_proc[i] = open_proc.get(&f).copied();
            }
        }
    }
    if line_mod.iter().all(Option::is_none) {
        return text.to_string(); // no module owns any line — exact passthrough
    }

    // `.globl`/`.global` names stay global no matter where they sit.
    let mut exported: HashSet<&str> = HashSet::new();
    for raw in &lines {
        let t = strip_comment(raw).trim();
        if let Some(rest) = t.strip_prefix(".globl").or_else(|| t.strip_prefix(".global")) {
            exported.insert(rest.trim());
        }
    }

    // Collect private labels: proc-internal ones into proc_map[proc] (`proc$label`),
    // module-level ones into name_map[module] (`Module$label`). Capital / `.globl`'d
    // names are exported and collected by neither.
    let mut name_map: HashMap<&str, HashMap<String, String>> = HashMap::new();
    let mut proc_map: HashMap<&str, HashMap<String, String>> = HashMap::new();
    for i in 0..lines.len() {
        let Some(m) = line_mod[i] else { continue };
        let Some(lab) = module_label_def(strip_comment(lines[i]).trim()) else { continue };
        let cap = lab.chars().next().map_or(false, |c| c.is_ascii_uppercase());
        if cap || exported.contains(lab) {
            continue;
        }
        match line_proc[i] {
            Some(p) => {
                proc_map
                    .entry(p)
                    .or_default()
                    .entry(lab.to_string())
                    .or_insert_with(|| format!("{p}${lab}"));
            }
            None => {
                name_map
                    .entry(m)
                    .or_default()
                    .entry(lab.to_string())
                    .or_insert_with(|| format!("{m}${lab}"));
            }
        }
    }

    // A proc's effective rename map = its module's privates + its own (the proc wins on
    // a name clash, so a proc-internal label shadows a module-level one within it).
    let mut proc_eff: HashMap<&str, HashMap<String, String>> = HashMap::new();
    for (&p, pmap) in &proc_map {
        let m = proc_module.get(p).copied().unwrap_or("");
        let mut eff = name_map.get(m).cloned().unwrap_or_default();
        eff.extend(pmap.iter().map(|(k, v)| (k.clone(), v.clone())));
        proc_eff.insert(p, eff);
    }

    let mut out = String::with_capacity(text.len() + 32);
    for (i, raw) in lines.iter().enumerate() {
        // Inner-most scope first: the proc's map (which already folds in the module's),
        // else the module's map, else verbatim.
        let map = line_proc[i]
            .and_then(|p| proc_eff.get(p))
            .or_else(|| line_mod[i].and_then(|m| name_map.get(m)));
        match map {
            Some(m) if !m.is_empty() => out.push_str(&rename_local_labels(raw, m)),
            _ => out.push_str(raw),
        }
        out.push('\n');
    }
    out
}

/// The label a line DEFINES, if any: a `name:` code label or a `name TYPE …` data
/// label (no colon). Used to find a module's private symbols.
fn module_label_def(t: &str) -> Option<&str> {
    if let Some(l) = leading_label(t) {
        return Some(l);
    }
    if let Some((Some(l), _, _)) = parse_data_line(t) {
        return Some(l);
    }
    None
}

/// Rename label definitions + references on one code line using `map`, leaving the
/// mnemonic/directive, any trailing comment, strings and indentation untouched.
/// Structured like [`rewrite_line`] so a mnemonic is never mistaken for a label.
fn rename_local_labels(raw: &str, map: &HashMap<String, String>) -> String {
    if map.is_empty() {
        return raw.to_string();
    }
    // Whole-word-substitute the private label names across the line's CODE (the part
    // before any comment), skipping string/char literals. This renames a definition
    // (`bdAlpha:`, `fb BYTE …`) and every reference uniformly — including a label in
    // the FIRST token, as in a `pObj.Method(label, …)` COM call (which a mnemonic /
    // operand split would miss). Mnemonics, registers, struct types and constants
    // never match a private label name in `map`, so they pass through untouched.
    let code = strip_comment(raw);
    let comment = &raw[code.len()..];
    let subs: Vec<(&str, String)> = map.iter().map(|(f, t)| (f.as_str(), t.clone())).collect();
    format!("{}{}", substitute(code, &subs), comment)
}

pub fn lower(src: &str, kb: &Kb) -> Result<String> {
    Ok(lower_mapped(src, kb)?.0)
}

/// Rewrite a leading ``line N: `` tag on an error so `N` (a *lowered* line number,
/// as produced by `rasm::assemble`) is mapped back through `map` to its real
/// source line. Used by the compile path so a downstream encode error points at
/// the source line, not the post-preprocessing/lowering line. Mirrors the
/// remapping `check` does on its diagnostics.
pub fn remap_assemble_error(e: anyhow::Error, map: &[usize]) -> anyhow::Error {
    let (lowered, msg) = split_line_tag(&format!("{e:#}"));
    match lowered.and_then(|n| map.get(n.saturating_sub(1)).copied()) {
        Some(src_line) => anyhow::anyhow!("line {src_line}: {msg}"),
        None => e,
    }
}

/// Lower `src`, also returning a map from each *lowered* line (0-based) back to
/// the 1-based source line it came from. One source line can expand to many
/// lowered lines (an `invoke`, a `.while`, or a user macro), so this is what lets
/// [`check`] point a downstream `rasm::assemble` error — whose line numbers are
/// lowered-line numbers — at the real source line.
///
/// User macros are expanded first (a separate textual pass); the two line maps
/// compose, so a macro's body still maps back to the invocation line.
pub fn lower_mapped(src: &str, kb: &Kb) -> Result<(String, Vec<usize>)> {
    let (expanded, emap) = expand_macros(src)?;
    // Compile-time layer: fold MASM equates (`NAME equ …` / `NAME = …`) and
    // resolve undotted conditional assembly (`IF`/`IFDEF`/`ELSE`/`ENDIF`) before
    // anything structural is lowered. Returns its own 1-based map (expanded line
    // per emitted line) so a downstream error still points at the real source.
    let (preprocessed, pmap) = preprocess_equates(&expanded, kb)?;
    let (lowered, lmap) = lower_expanded(&preprocessed, kb)?;
    // Compose all three maps: lowered → preprocessed → expanded → source.
    let map = lmap
        .into_iter()
        .map(|ll| {
            let pl = pmap.get(ll.wrapping_sub(1)).copied().unwrap_or(0);
            emap.get(pl.wrapping_sub(1)).copied().unwrap_or(0)
        })
        .collect();
    Ok((lowered, map))
}

// ── compile-time equates + conditional assembly (MASM-style) ──────────────────
//
// A textual pass that runs after macro expansion and before structural lowering.
// It does two interleaved things, top-to-bottom in one walk so each can see the
// other:
//
//   * EQUATES — `NAME equ <expr>` and `NAME = <expr>` define an integer constant
//     (define-before-use). The definition line emits nothing; every later use of
//     `NAME` as a whole word (outside strings and comments) is replaced by its
//     value. This is what lets a `dup` count fold to a literal before `lower_data`
//     ever sees it.
//
//   * CONDITIONAL ASSEMBLY — undotted `IF`/`IFDEF`/`IFNDEF`/`ELSEIF`/`ELSE`/
//     `ENDIF`. These are COMPILE-TIME and entirely distinct from the runtime
//     `.if` (which lowers to a compare + branch). Excluded lines emit nothing; an
//     equate inside a skipped branch is never recorded.
//
// `=` is overloaded: it is also a struct field assignment inside a
// `LABEL struct TYPE … field = value … ends` block. We track that block depth and
// treat a bare `=` as an equate ONLY outside such a block. `equ` is always an
// equate.

/// One frame of the conditional-assembly stack.
struct CondFrame {
    /// Are lines in the *current* clause emitted?
    active: bool,
    /// Has any clause in this IF chain been taken yet? (gates ELSEIF/ELSE.)
    taken: bool,
    /// Have we already seen the `ELSE`? (a second `ELSE`/`ELSEIF` is an error.)
    else_seen: bool,
    /// Was the enclosing context emitting when this IF opened? A nested IF inside
    /// a skipped branch stays skipped regardless of its own condition.
    parent_active: bool,
    /// 1-based source line of the opening `IF`/`IFDEF`/`IFNDEF`, so an unclosed
    /// frame can point its diagnostic at the dangling opener.
    open_line: usize,
}

/// Recursive-descent integer evaluator over equates + winkb constants.
///
/// Grammar (lowest precedence first), matching C / MASM expression rules:
///   or    := xor   ('|' xor)*
///   xor   := and   ('^' and)*
///   and   := shift ('&' shift)*
///   shift := add   (('<<'|'>>') add)*
///   add   := mul   (('+'|'-') mul)*
///   mul   := unary (('*'|'/'|'%') unary)*
///   unary := ('-'|'~')* primary
///   primary := NUMBER | IDENT | '(' or ')'
///
/// A NUMBER is decimal or `0x`-hex (underscores allowed). An IDENT resolves as
/// (a) an equate from `eqs`, else (b) a winkb constant via `kb.resolve`, else a
/// hard error — a compile-time expression must be fully known.
fn eval_expr(expr: &str, eqs: &HashMap<String, i64>, kb: &Kb) -> Result<i64> {
    let toks = tokenize_expr(expr)?;
    let mut p = ExprParser { toks: &toks, pos: 0, eqs, kb };
    let v = p.parse_or()?;
    if p.pos != p.toks.len() {
        bail!("trailing tokens in expression `{}`", expr.trim());
    }
    Ok(v)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
}

/// Lex an expression into tokens. Multi-char operators `<<` and `>>` are matched
/// before their single-char prefixes.
fn tokenize_expr(s: &str) -> Result<Vec<Tok>> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '<' && i + 1 < b.len() && b[i + 1] == b'<' {
            out.push(Tok::Op("<<"));
            i += 2;
            continue;
        }
        if c == '>' && i + 1 < b.len() && b[i + 1] == b'>' {
            out.push(Tok::Op(">>"));
            i += 2;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '+' | '-' | '*' | '/' | '%' | '&' | '^' | '|' | '~' => {
                let op: &'static str = match c {
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '%' => "%",
                    '&' => "&",
                    '^' => "^",
                    '|' => "|",
                    '~' => "~",
                    _ => unreachable!(),
                };
                out.push(Tok::Op(op));
                i += 1;
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < b.len()
                    && (is_ident_char(b[i] as char) || b[i] as char == '_')
                {
                    i += 1;
                }
                let raw = &s[start..i];
                let body = raw.replace('_', "");
                let n = if let Some(h) =
                    body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
                {
                    i64::from_str_radix(h, 16)
                        .with_context(|| format!("bad hex literal `{raw}`"))?
                } else {
                    body.parse::<i64>()
                        .with_context(|| format!("bad integer literal `{raw}`"))?
                };
                out.push(Tok::Num(n));
            }
            _ if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < b.len() && is_ident_char(b[i] as char) {
                    i += 1;
                }
                out.push(Tok::Ident(s[start..i].to_string()));
            }
            _ => bail!("unexpected character `{c}` in expression"),
        }
    }
    Ok(out)
}

struct ExprParser<'a> {
    toks: &'a [Tok],
    pos: usize,
    eqs: &'a HashMap<String, i64>,
    kb: &'a Kb,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn parse_or(&mut self) -> Result<i64> {
        let mut v = self.parse_xor()?;
        while self.eat_op("|") {
            v |= self.parse_xor()?;
        }
        Ok(v)
    }
    fn parse_xor(&mut self) -> Result<i64> {
        let mut v = self.parse_and()?;
        while self.eat_op("^") {
            v ^= self.parse_and()?;
        }
        Ok(v)
    }
    fn parse_and(&mut self) -> Result<i64> {
        let mut v = self.parse_shift()?;
        while self.eat_op("&") {
            v &= self.parse_shift()?;
        }
        Ok(v)
    }
    fn parse_shift(&mut self) -> Result<i64> {
        let mut v = self.parse_add()?;
        loop {
            if self.eat_op("<<") {
                let s = self.parse_add()?;
                if !(0..64).contains(&s) {
                    bail!("shift count {s} out of range 0..=63");
                }
                v = v.wrapping_shl(s as u32);
            } else if self.eat_op(">>") {
                let s = self.parse_add()?;
                if !(0..64).contains(&s) {
                    bail!("shift count {s} out of range 0..=63");
                }
                // Logical (MASM `SHR`) shift: a negative left operand does not
                // sign-extend, so masking flags built from `~0`/negatives folds the
                // way an asm writer reaching for SHR expects.
                v = ((v as u64) >> (s as u32)) as i64;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_add(&mut self) -> Result<i64> {
        let mut v = self.parse_mul()?;
        loop {
            if self.eat_op("+") {
                v = v.wrapping_add(self.parse_mul()?);
            } else if self.eat_op("-") {
                v = v.wrapping_sub(self.parse_mul()?);
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_mul(&mut self) -> Result<i64> {
        let mut v = self.parse_unary()?;
        loop {
            if self.eat_op("*") {
                v = v.wrapping_mul(self.parse_unary()?);
            } else if self.eat_op("/") {
                let d = self.parse_unary()?;
                if d == 0 {
                    bail!("division by zero in expression");
                }
                v = v
                    .checked_div(d)
                    .ok_or_else(|| anyhow::anyhow!("division overflow (i64::MIN / -1)"))?;
            } else if self.eat_op("%") {
                let d = self.parse_unary()?;
                if d == 0 {
                    bail!("modulo by zero in expression");
                }
                v = v
                    .checked_rem(d)
                    .ok_or_else(|| anyhow::anyhow!("modulo overflow (i64::MIN % -1)"))?;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_unary(&mut self) -> Result<i64> {
        if self.eat_op("-") {
            return Ok(self.parse_unary()?.wrapping_neg());
        }
        if self.eat_op("~") {
            return Ok(!self.parse_unary()?);
        }
        self.parse_primary()
    }
    fn parse_primary(&mut self) -> Result<i64> {
        match self.peek() {
            Some(Tok::Num(n)) => {
                let n = *n;
                self.pos += 1;
                Ok(n)
            }
            Some(Tok::Ident(name)) => {
                let name = name.clone();
                self.pos += 1;
                if let Some(v) = self.eqs.get(&name) {
                    return Ok(*v);
                }
                if let Some(v) = self.kb.resolve(&name)?.first() {
                    return Ok(v.i64v);
                }
                bail!("unknown name `{name}` in expression")
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let v = self.parse_or()?;
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    bail!("missing `)` in expression");
                }
                self.pos += 1;
                Ok(v)
            }
            other => bail!("unexpected token {other:?} in expression"),
        }
    }
}

/// `NAME equ <expr>` or `NAME = <expr>` → `(NAME, expr)`, if `t` is a definition.
/// `in_struct` suppresses the bare-`=` form (it's a struct field there). `equ` is
/// recognised everywhere. The `=`/`equ` keyword must be whole-word, and NAME a
/// valid asm identifier.
fn parse_equate<'a>(t: &'a str, in_struct: bool) -> Option<(&'a str, &'a str)> {
    // `NAME equ rest`
    let (name, rest) = split_first(t);
    if !name.is_empty() && name.chars().all(is_ident_char) && !name.chars().next().unwrap().is_ascii_digit() {
        if let Some(expr) = strip_keyword(rest, "equ") {
            if !expr.is_empty() {
                return Some((name, expr));
            }
        }
    }
    // `NAME = rest` — only outside a struct block.
    if !in_struct {
        if let Some((lhs, rhs)) = t.split_once('=') {
            let lhs = lhs.trim();
            let rhs = rhs.trim();
            // Reject comparison / compound operators that merely contain `=`
            // (`==`, `<=`, `>=`, `!=`); a bare equate's lhs is a single ident.
            let after = &t[t.find('=').unwrap() + 1..];
            if !lhs.is_empty()
                && lhs.chars().all(is_ident_char)
                && !lhs.chars().next().unwrap().is_ascii_digit()
                && !after.starts_with('=')
                && !lhs.ends_with(['<', '>', '!', '='])
                && !rhs.is_empty()
            {
                return Some((lhs, rhs));
            }
        }
    }
    None
}

/// The equates a source defines, as `(name, 1-based line)`. `check()` registers
/// these as known names so a defined equate isn't flagged as an unknown constant,
/// and notes any whose name coincides with a Windows constant (a shadow). Mirrors
/// `preprocess_equates`'s recognition via `parse_equate` + struct/raw-block tracking.
fn collect_equate_defs(src: &str) -> Vec<(String, usize)> {
    let mut defs = Vec::new();
    let mut in_struct = false;
    let mut raw_end: Option<&str> = None;
    for (i, raw) in src.lines().enumerate() {
        let t = strip_comment(raw).trim();
        if let Some(end) = raw_end {
            if raw.trim().eq_ignore_ascii_case(end) {
                raw_end = None;
            }
            continue;
        }
        if t.eq_ignore_ascii_case(".asciistring") {
            raw_end = Some(".endasciistring");
            continue;
        }
        if t.eq_ignore_ascii_case(".widestring") {
            raw_end = Some(".endwidestring");
            continue;
        }
        if parse_struct_open(t).is_some() {
            in_struct = true;
            continue;
        }
        if t == "ends" || t == "endstruct" {
            in_struct = false;
            continue;
        }
        if let Some((name, _)) = parse_equate(t, in_struct) {
            defs.push((name.to_string(), i + 1));
        }
    }
    defs
}

/// One user-defined equate, surfaced for the IDE: its name, the *source*
/// expression text it was defined with, the folded `i64` value that expression
/// evaluates to, and the 1-based source line of the definition. The IDE uses this
/// for hover ("`CELLS = WIDTH * HEIGHT` = **2000**") and go-to-definition.
#[derive(Debug, Clone)]
pub struct EquateDef {
    pub name: String,
    pub expr: String,
    pub value: i64,
    pub line: usize,
}

/// Every equate a buffer defines, with its source expression, its folded value,
/// and its 1-based line. Uses exactly the recognition of [`preprocess_equates`]
/// (`parse_equate` + struct / raw-string-block tracking) and accumulates an `eqs`
/// map top-to-bottom so a later equate that references an earlier one — e.g.
/// `A equ W * H` — folds correctly (define-before-use).
///
/// Unlike `preprocess_equates` this is *non-failing*: an equate whose expression
/// doesn't evaluate (an unknown name, a syntax error mid-edit) is simply skipped
/// rather than aborting the whole table — the IDE wants whatever it can resolve.
/// Register-shadowing equates (which `preprocess_equates` rejects) are likewise
/// dropped here, since they never become real equates.
pub fn equate_table(src: &str, kb: &Kb) -> Vec<EquateDef> {
    let mut eqs: HashMap<String, i64> = HashMap::new();
    let mut out = Vec::new();
    let mut in_struct = false;
    let mut raw_end: Option<&str> = None;
    for (i, raw) in src.lines().enumerate() {
        let t = strip_comment(raw).trim();
        if let Some(end) = raw_end {
            if raw.trim().eq_ignore_ascii_case(end) {
                raw_end = None;
            }
            continue;
        }
        if t.eq_ignore_ascii_case(".asciistring") {
            raw_end = Some(".endasciistring");
            continue;
        }
        if t.eq_ignore_ascii_case(".widestring") {
            raw_end = Some(".endwidestring");
            continue;
        }
        if parse_struct_open(t).is_some() {
            in_struct = true;
            continue;
        }
        if t == "ends" || t == "endstruct" {
            in_struct = false;
            continue;
        }
        if let Some((name, expr)) = parse_equate(t, in_struct) {
            // A register-named equate is rejected by the real preprocessor; it
            // never resolves to a usable equate, so don't surface one.
            if is_register(name) {
                continue;
            }
            // The rhs may reference earlier equates by name; substitute first
            // (so `A equ W * H` works), then fold. Keep the *original* expr text.
            let sub = substitute_equates(expr, &eqs);
            if let Ok(v) = eval_expr(&sub, &eqs, kb) {
                eqs.insert(name.to_string(), v);
                out.push(EquateDef {
                    name: name.to_string(),
                    expr: expr.to_string(),
                    value: v,
                    line: i + 1,
                });
            }
        }
    }
    out
}

/// Interpolate `{{NAME}}` placeholders inside a string span with the equate's
/// integer value (decimal). Everything else — including multibyte UTF-8 — is copied
/// verbatim. An unknown `{{NAME}}` (or one with no closing `}}`) is left untouched,
/// so a literal `{{` only "activates" when it names a real equate.
fn interpolate_equates(span: &str, eqs: &HashMap<String, i64>) -> String {
    if !span.contains("{{") {
        return span.to_string();
    }
    let mut out = String::new();
    let mut rest = span;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                let name = after[..close].trim();
                match eqs.get(name) {
                    Some(v) => out.push_str(&v.to_string()),
                    None => {
                        out.push_str("{{");
                        out.push_str(&after[..close]);
                        out.push_str("}}");
                    }
                }
                rest = &after[close + 2..];
            }
            None => {
                out.push_str("{{");
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replace whole-word equate names with their integer value, skipping the insides
/// of double-quoted strings and (already stripped) comments. Single-char `'…'`
/// literals are copied verbatim too. A `"…"` string passes through
/// [`interpolate_equates`] so `{{NAME}}` placeholders fold.
fn substitute_equates(line: &str, eqs: &HashMap<String, i64>) -> String {
    if eqs.is_empty() {
        return line.to_string();
    }
    let b = line.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c == '"' || c == '\'' {
            // Copy the quoted span verbatim by BYTE RANGE (not byte-by-byte): a
            // multibyte UTF-8 char inside the string — e.g. an em-dash `—` in a
            // WCHAR title — must survive intact. `b[i] as char` would split it into
            // its raw bytes and re-encode them as separate code points.
            let start = i;
            i += 1;
            while i < b.len() {
                let done = b[i] == c as u8;
                i += 1;
                if done {
                    break;
                }
            }
            let span = &line[start..i];
            // A "double-quoted" string interpolates `{{NAME}}`; a 'char' literal is
            // copied verbatim.
            if c == '"' {
                out.push_str(&interpolate_equates(span, eqs));
            } else {
                out.push_str(span);
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && is_ident_char(b[i] as char) {
                i += 1;
            }
            let tok = &line[start..i];
            match eqs.get(tok) {
                Some(v) => out.push_str(&v.to_string()),
                None => out.push_str(tok),
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The compile-time preprocessing pass. Returns the rewritten text and a 1-based
/// map from each *output* line to the *input* (expanded) line it came from, so a
/// downstream error still points at the real source.
fn preprocess_equates(src: &str, kb: &Kb) -> Result<(String, Vec<usize>)> {
    let mut eqs: HashMap<String, i64> = HashMap::new();
    let mut out = String::new();
    let mut map: Vec<usize> = Vec::new();
    let mut conds: Vec<CondFrame> = Vec::new();
    // Depth of the data `LABEL struct TYPE … ends` block, where `=` is a field
    // assignment rather than an equate. (`struct` data blocks do not nest, but a
    // counter keeps the bookkeeping simple and robust.)
    let mut struct_depth = 0usize;
    // Inside a `.ASCIISTRING … .ENDASCIISTRING` (or `.WIDESTRING …`) raw block the
    // body is verbatim shader/text source — it must NOT be scanned for equates or
    // conditionals (a line like `ax=clamp(ax, x0, …);` is HLSL, not a `NAME = expr`
    // equate). Copy every such line through untouched until the matching `.end…`.
    let mut raw_block_end: Option<&'static str> = None;

    let emitting = |conds: &[CondFrame]| conds.iter().all(|f| f.active);

    for (i, raw) in src.lines().enumerate() {
        let in_line = i + 1;
        let t = strip_comment(raw).trim();
        let active = emitting(&conds);

        // ── raw string block: pass every line through verbatim (including the
        //    opening/closing directives) so structural lowering still sees it ───
        if let Some(end_kw) = raw_block_end {
            out.push_str(raw);
            out.push('\n');
            map.push(in_line);
            if raw.trim().eq_ignore_ascii_case(end_kw) {
                raw_block_end = None;
            }
            continue;
        }
        if active && t.eq_ignore_ascii_case(".asciistring") {
            raw_block_end = Some(".endasciistring");
            out.push_str(raw);
            out.push('\n');
            map.push(in_line);
            continue;
        }
        if active && t.eq_ignore_ascii_case(".widestring") {
            raw_block_end = Some(".endwidestring");
            out.push_str(raw);
            out.push('\n');
            map.push(in_line);
            continue;
        }

        // ── conditional-assembly directives (handled even while skipping, so the
        //    nesting stays balanced) ──────────────────────────────────────────
        if let Some(expr) = strip_keyword_ci(t, "if") {
            let parent = active;
            let val = if parent { eval_expr(expr, &eqs, kb).map_err(|e| line_err(in_line, e))? } else { 0 };
            conds.push(CondFrame {
                active: parent && val != 0,
                taken: parent && val != 0,
                else_seen: false,
                parent_active: parent,
                open_line: in_line,
            });
            continue;
        }
        if let Some(name) = strip_keyword_ci(t, "ifdef") {
            let parent = active;
            // MASM IFDEF takes exactly one symbol — resolve the first token, so a
            // stray trailing token can't make a defined name look undefined.
            let def = eqs.contains_key(split_first(name.trim()).0);
            conds.push(CondFrame {
                active: parent && def,
                taken: parent && def,
                else_seen: false,
                parent_active: parent,
                open_line: in_line,
            });
            continue;
        }
        if let Some(name) = strip_keyword_ci(t, "ifndef") {
            let parent = active;
            let undef = !eqs.contains_key(split_first(name.trim()).0);
            conds.push(CondFrame {
                active: parent && undef,
                taken: parent && undef,
                else_seen: false,
                parent_active: parent,
                open_line: in_line,
            });
            continue;
        }
        if let Some(expr) = strip_keyword_ci(t, "elseif") {
            let f = conds
                .last_mut()
                .ok_or_else(|| line_err(in_line, anyhow::anyhow!("`ELSEIF` without an open `IF`")))?;
            if f.else_seen {
                return Err(line_err(in_line, anyhow::anyhow!("`ELSEIF` after `ELSE`")));
            }
            let parent = f.parent_active;
            if f.taken || !parent {
                f.active = false;
            } else {
                let val = eval_expr(expr, &eqs, kb).map_err(|e| line_err(in_line, e))?;
                f.active = val != 0;
                f.taken = val != 0;
            }
            continue;
        }
        if t.eq_ignore_ascii_case("else") {
            let f = conds
                .last_mut()
                .ok_or_else(|| line_err(in_line, anyhow::anyhow!("`ELSE` without an open `IF`")))?;
            if f.else_seen {
                return Err(line_err(in_line, anyhow::anyhow!("duplicate `ELSE`")));
            }
            f.else_seen = true;
            f.active = f.parent_active && !f.taken;
            f.taken = true;
            continue;
        }
        if t.eq_ignore_ascii_case("endif") {
            if conds.pop().is_none() {
                return Err(line_err(in_line, anyhow::anyhow!("`ENDIF` without an open `IF`")));
            }
            continue;
        }

        // Excluded by a false branch: emit nothing, define nothing.
        if !active {
            continue;
        }

        // ── struct-block depth tracking (so `=` means the right thing) ────────
        if parse_struct_open(t).is_some() {
            struct_depth += 1;
        }
        let in_struct = struct_depth > 0;
        if in_struct && (t == "ends" || t == "endstruct") {
            struct_depth = struct_depth.saturating_sub(1);
        }

        // ── equate definition: record, emit nothing ─────────────────────────
        if let Some((name, expr)) = parse_equate(t, in_struct) {
            // A register-named equate would silently rewrite `mov eax, 1` into
            // `mov 99, 1` — exactly the kind of hidden corruption the dialect
            // refuses to do. Reject it with a clear, line-tagged error instead.
            if is_register(name) {
                return Err(line_err(
                    in_line,
                    anyhow::anyhow!("equate `{name}` shadows a register name"),
                ));
            }
            // The rhs may itself reference earlier equates by name; substitute
            // first so `B equ A + 1` works, then evaluate.
            let sub = substitute_equates(expr, &eqs);
            let v = eval_expr(&sub, &eqs, kb).map_err(|e| line_err(in_line, e))?;
            eqs.insert(name.to_string(), v);
            continue;
        }

        // ── ordinary line: substitute equate uses, keep verbatim otherwise ──
        // Substitute only over the code portion; re-append the original comment
        // verbatim so equate names inside a comment are never rewritten (and a
        // stray apostrophe in comment text can't flip quote-tracking).
        let code = strip_comment(raw);
        out.push_str(&substitute_equates(code, &eqs));
        out.push_str(&raw[code.len()..]);
        out.push('\n');
        map.push(in_line);
    }
    if let Some(f) = conds.first() {
        return Err(line_err(
            f.open_line,
            anyhow::anyhow!("`IF` without a matching `ENDIF`"),
        ));
    }
    Ok((out, map))
}

/// Prefix an error with `line N:` (matching the convention `split_line_tag` peels
/// back), so a preprocessing failure points at its source line.
fn line_err(line: usize, e: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("line {line}: {e:#}")
}

/// Like [`strip_keyword`] but case-insensitive (MASM `IF`/`if`/`Ifdef` all work).
fn strip_keyword_ci<'a>(t: &'a str, kw: &str) -> Option<&'a str> {
    let (head, rest) = split_first(t);
    if head.eq_ignore_ascii_case(kw) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// Lower already-macro-expanded `src`. The returned map is the 1-based *expanded*
/// line per lowered line; [`lower_mapped`] composes it back to source lines.
fn lower_expanded(src: &str, kb: &Kb) -> Result<(String, Vec<usize>)> {
    let mut labels = collect_labels(src);
    let com_objs = collect_com_objs(src); // `comobj name : Interface` bindings
    labels.extend(com_objs.keys().cloned());
    let mut out = String::new();
    let mut map: Vec<usize> = Vec::new();
    // High-level block state: a counter for unique labels, and a stack so each
    // `.endX` matches its opener and `.break`/`.continue` find the inner loop.
    let mut block_ctr = 0usize;
    let mut block_stack: Vec<Block> = Vec::new();
    let mut pending_struct: Option<StructAccum> = None;
    let mut pending_string: Option<StringAccum> = None;
    // When set, the lowered lines this source line produced are pure data with no
    // instructive value (a raw string block) — map them to source line 0 so the
    // grey ghost view shows nothing for them.
    let mut ghost_suppress = false;
    // Pre-pass: size each `frame` proc's reserved area (shadow + the largest
    // outgoing-arg list in its body), keyed by the proc's 1-based line.
    let mut proc_frame: HashMap<usize, usize> = HashMap::new();
    {
        let mut start = None;
        let mut framed = false;
        let mut max_args = 0;
        for (i, raw) in src.lines().enumerate() {
            let t = strip_comment(raw).trim();
            if let Some((rest, fp)) = strip_proc_kw(t) {
                let h = parse_proc(rest, fp);
                start = Some(i + 1);
                framed = h.frame;
                max_args = 0;
            } else if t == "endproc" {
                if framed {
                    if let Some(s) = start {
                        proc_frame.insert(s, proc_frame_size(max_args));
                    }
                }
                start = None;
                framed = false;
            } else if start.is_some() && framed {
                if let Some(c) = call_arg_count(t) {
                    max_args = max_args.max(c);
                }
            }
        }
    }
    for (i, raw) in src.lines().enumerate() {
        let src_line = i + 1;
        let start = out.len();
        let body = strip_comment(raw);
        let t = body.trim();
        if let Some(acc) = pending_string.as_mut() {
            // Inside a raw string block: capture every line verbatim (comments and
            // all) until the matching `.end…`, which flushes it to data.
            if raw.trim().eq_ignore_ascii_case(acc.end_kw) {
                let acc = pending_string.take().unwrap();
                out.push_str(&emit_string_block(&acc));
                ghost_suppress = true;
            } else {
                acc.lines.push(raw.to_string());
            }
        } else if t.eq_ignore_ascii_case(".asciistring") {
            pending_string = Some(StringAccum { lines: Vec::new(), wide: false, end_kw: ".endasciistring" });
        } else if t.eq_ignore_ascii_case(".widestring") {
            pending_string = Some(StringAccum { lines: Vec::new(), wide: true, end_kw: ".endwidestring" });
        } else if pending_struct.is_some() {
            // Inside a `LABEL struct TYPE … ends` data block: collect each
            // `field = value`, then lay the whole thing out on `ends`.
            if t == "ends" || t == "endstruct" {
                let acc = pending_struct.take().unwrap();
                out.push_str(
                    &emit_struct(kb, &acc)
                        .with_context(|| format!("line {src_line}: struct {}", acc.label))?,
                );
            } else if !t.is_empty() {
                let (lhs, rhs) = t.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("line {src_line}: struct field needs `name = value`, got `{t}`")
                })?;
                pending_struct
                    .as_mut()
                    .unwrap()
                    .fields
                    .push((lhs.trim().to_string(), rhs.trim().to_string()));
            }
        } else if t.is_empty() {
            out.push('\n');
        } else if strip_keyword(t, "module").is_some() || t == "endmodule" {
            // Module markers: label scoping is resolved before lowering (see
            // `scope_module_labels`), so here they are no-ops — emit a blank line to
            // keep the source-line map aligned.
            out.push('\n');
        } else if let Some((label, ty)) = parse_struct_open(t) {
            pending_struct = Some(StructAccum { label, ty, fields: Vec::new() });
        } else if let Some(rest) = strip_keyword(t, "invoke") {
            // `invoke Func, args…`
            let expanded = expand_invoke(rest, kb, &labels, in_framed_proc(&block_stack))
                .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?;
            out.push_str(&expanded);
        } else if let Some(rest) = strip_keyword(t, "comcall") {
            // `comcall obj, Interface, Method, args…` — COM vtable call (db-aware)
            let expanded = expand_comcall(rest, kb, &labels, in_framed_proc(&block_stack))
                .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?;
            out.push_str(&expanded);
        } else if let Some(rest) = strip_keyword(t, "iid") {
            // `iid Interface` — emit that interface's IID as 16 GUID bytes
            let expanded =
                emit_iid(rest, kb).with_context(|| format!("line {src_line}: `{}`", raw.trim()))?;
            out.push_str(&expanded);
        } else if let Some(rest) = strip_keyword(t, "comobj") {
            // `comobj NAME : Interface` — a pointer slot (binding is pre-scanned)
            let name = rest.split_once(':').map_or(rest.trim(), |(n, _)| n.trim());
            out.push_str(&format!("{name}:\n  .zero 8\n"));
        } else if let Some((obj, iface, method, args)) = parse_method_call(t, &com_objs) {
            // `obj.Method(args)` — typed COM call on the bound interface.
            let rest = if args.is_empty() {
                format!("[rip + {obj}], {iface}, {method}")
            } else {
                format!("[rip + {obj}], {iface}, {method}, {args}")
            };
            let expanded = expand_comcall(&rest, kb, &labels, in_framed_proc(&block_stack))
                .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?;
            out.push_str(&expanded);
        } else if let Some(cond) = strip_keyword(t, ".if") {
            // `.if c` → test c; on false skip to the next clause.
            let id = block_ctr;
            block_ctr += 1;
            block_stack.push(Block::If { id, next: 1, else_seen: false });
            out.push_str(
                &cond_jump(cond, &format!("__if{id}_1"), false, kb, &labels)
                    .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?,
            );
        } else if let Some(cond) = strip_keyword(t, ".elseif") {
            // End the previous clause's body, land its skip, then test this one.
            let (id, cur) = match block_stack.last_mut() {
                Some(Block::If { id, next, else_seen }) if !*else_seen => {
                    let r = (*id, *next);
                    *next += 1;
                    r
                }
                _ => bail!("line {src_line}: `.elseif` without an open `.if`"),
            };
            out.push_str(&format!("  jmp __if{id}_end\n__if{id}_{cur}:\n"));
            out.push_str(
                &cond_jump(cond, &format!("__if{id}_{}", cur + 1), false, kb, &labels)
                    .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?,
            );
        } else if t == ".else" {
            let (id, cur) = match block_stack.last_mut() {
                Some(Block::If { id, next, else_seen }) if !*else_seen => {
                    *else_seen = true;
                    (*id, *next)
                }
                _ => bail!("line {src_line}: `.else` without an open `.if`"),
            };
            out.push_str(&format!("  jmp __if{id}_end\n__if{id}_{cur}:\n"));
        } else if t == ".endif" {
            match block_stack.pop() {
                // No `.else`: the last clause's skip lands here, which is the end.
                Some(Block::If { id, next, else_seen: false }) => {
                    out.push_str(&format!("__if{id}_{next}:\n__if{id}_end:\n"))
                }
                Some(Block::If { id, else_seen: true, .. }) => {
                    out.push_str(&format!("__if{id}_end:\n"))
                }
                _ => bail!("line {src_line}: `.endif` without an open `.if`"),
            }
        } else if let Some(cond) = strip_keyword(t, ".while") {
            // `.while c` → top label, the test, and the exit branch.
            let id = block_ctr;
            block_ctr += 1;
            block_stack.push(Block::While { id });
            out.push_str(&format!("__while{id}_top:\n"));
            out.push_str(
                &cond_jump(cond, &format!("__while{id}_end"), false, kb, &labels)
                    .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?,
            );
        } else if t == ".endw" {
            match block_stack.pop() {
                Some(Block::While { id }) => {
                    out.push_str(&format!("  jmp __while{id}_top\n__while{id}_end:\n"))
                }
                _ => bail!("line {src_line}: `.endw` without an open `.while`"),
            }
        } else if t == ".repeat" {
            // `.repeat` … `.until c` → run the body, then loop back while c false.
            let id = block_ctr;
            block_ctr += 1;
            block_stack.push(Block::Repeat { id });
            out.push_str(&format!("__repeat{id}_top:\n"));
        } else if let Some(cond) = strip_keyword(t, ".until") {
            match block_stack.pop() {
                Some(Block::Repeat { id }) => {
                    out.push_str(&format!("__repeat{id}_test:\n"));
                    out.push_str(
                        &cond_jump(cond, &format!("__repeat{id}_top"), false, kb, &labels)
                            .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?,
                    );
                    out.push_str(&format!("__repeat{id}_end:\n"));
                }
                _ => bail!("line {src_line}: `.until` without an open `.repeat`"),
            }
        } else if let Some(rest) = strip_keyword(t, ".for") {
            // `.for reg = start to end` → init, top label, test, exit branch.
            let (reg, range) = rest.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("line {src_line}: `.for` wants `reg = start to end`")
            })?;
            let (start, end) = range.split_once(" to ").ok_or_else(|| {
                anyhow::anyhow!("line {src_line}: `.for` wants `start to end`")
            })?;
            let reg = resolve_operands(reg.trim(), kb, &labels)?;
            let start = resolve_operands(start.trim(), kb, &labels)?;
            let end = resolve_operands(end.trim(), kb, &labels)?;
            let id = block_ctr;
            block_ctr += 1;
            out.push_str(&format!(
                "  mov {reg}, {start}\n__for{id}_top:\n  cmp {reg}, {end}\n  ja __for{id}_end\n"
            ));
            block_stack.push(Block::For { id, reg });
        } else if t == ".forever" {
            // `.forever` … `.endfor` → an infinite loop; leave only via `.break`.
            let id = block_ctr;
            block_ctr += 1;
            block_stack.push(Block::Forever { id });
            out.push_str(&format!("__forever{id}_top:\n"));
        } else if t == ".endfor" {
            match block_stack.pop() {
                Some(Block::For { id, reg }) => out.push_str(&format!(
                    "__for{id}_cont:\n  inc {reg}\n  jmp __for{id}_top\n__for{id}_end:\n"
                )),
                Some(Block::Forever { id }) => {
                    out.push_str(&format!("  jmp __forever{id}_top\n__forever{id}_end:\n"))
                }
                _ => bail!("line {src_line}: `.endfor` without an open `.for`/`.forever`"),
            }
        } else if let Some(rest) = strip_keyword(t, ".break") {
            out.push_str(&loop_jump(rest, true, &block_stack, kb, &labels, src_line, raw)?);
        } else if let Some(rest) = strip_keyword(t, ".continue") {
            out.push_str(&loop_jump(rest, false, &block_stack, kb, &labels, src_line, raw)?);
        } else if let Some((rest, fp)) = strip_proc_kw(t) {
            // `proc NAME uses R…` → the label + a visible prologue (push each saved
            // GP register, in order); `endproc`/`ret` pop them in reverse. `fproc`
            // additionally saves its non-volatile xmm `uses` (xmm6..xmm15) to a
            // movups'd stack area, restored in the epilogue. No frame → each
            // `invoke` inside aligns and shadow-spaces itself.
            if block_stack.iter().any(|b| matches!(b, Block::Proc { .. })) {
                bail!("line {src_line}: `proc` inside a `proc` — close the first with `endproc`");
            }
            let h = parse_proc(rest, fp);
            if h.name.is_empty() {
                bail!("line {src_line}: `proc` needs a name");
            }
            let (gpr_uses, xmm_uses) = split_uses(&h.uses);
            if !xmm_uses.is_empty() && !fp {
                bail!("line {src_line}: `proc {}` lists xmm in `uses`, but `uses` can only push GP registers — use `fproc` to save xmm6..xmm15", h.name);
            }
            let frame_size = proc_frame.get(&src_line).copied().unwrap_or(0);
            out.push_str(&format!("{}:\n", h.name));
            for r in &gpr_uses {
                out.push_str(&format!("  push {r}\n"));
            }
            // Reserve + movups-save the non-volatile xmm registers (alignment-free,
            // so it's correct whether or not this proc also sets up a frame).
            if !xmm_uses.is_empty() {
                out.push_str(&format!("  sub rsp, {}\n", xmm_uses.len() * 16));
                for (i, r) in xmm_uses.iter().enumerate() {
                    out.push_str(&format!("  movups [rsp + {}], {r}\n", i * 16));
                }
            }
            if frame_size > 0 {
                // Align the stack once with an rbp anchor, so the lean calls
                // inside are correct no matter how the caller entered us.
                out.push_str("  push rbp\n  mov rbp, rsp\n  and rsp, -16\n");
                out.push_str(&format!("  sub rsp, {frame_size}\n"));
            }
            block_stack.push(Block::Proc { uses: gpr_uses, xmm: xmm_uses, frame_size });
        } else if t == "endproc" {
            match block_stack.pop() {
                Some(Block::Proc { uses, xmm, frame_size }) => {
                    out.push_str(&proc_epilogue(&uses, &xmm, frame_size))
                }
                _ => bail!("line {src_line}: `endproc` without an open `proc`"),
            }
        } else if t == ".ret" || t == "ret" {
            // Inside a proc, a return releases the frame and restores the saved
            // registers first. `.ret` is the explicit early-exit form; a bare
            // `ret` is intercepted too so it can't skip the epilogue.
            match block_stack.iter().rev().find(|b| matches!(b, Block::Proc { .. })) {
                Some(Block::Proc { uses, xmm, frame_size }) => {
                    out.push_str(&proc_epilogue(uses, xmm, *frame_size))
                }
                _ if t == ".ret" => bail!("line {src_line}: `.ret` outside a `proc`"),
                _ => out.push_str("  ret\n"),
            }
        } else if include_path(raw).is_some() {
            // `.include` is expanded by `expand_includes()` before lowering; if one
            // reaches here unexpanded (e.g. the live IDE check), treat it as a no-op.
            out.push('\n');
        } else if t.starts_with('.') {
            // GAS directives (our high-level ones are handled above) pass through.
            out.push_str(body);
            out.push('\n');
        } else if let Some((label, type_kw, values)) = parse_data_line(t) {
            // MASM data: `[label] BYTE/WORD/DWORD/QWORD/WCHAR values`.
            out.push_str(
                &lower_data(label, type_kw, values, kb, &labels)
                    .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?,
            );
        } else {
            // Instruction (possibly with a leading `label:`): resolve operands.
            let line = rewrite_line(body, kb, &labels)
                .with_context(|| format!("line {src_line}: `{}`", raw.trim()))?;
            out.push_str(&line);
            out.push('\n');
        }
        // Every branch above ends each lowered line with '\n', so the count of
        // newlines just added is exactly how many lowered lines this source line
        // produced. (`rasm::assemble` enumerates the same lines() the same way.)
        let added = out[start..].bytes().filter(|&b| b == b'\n').count();
        map.resize(map.len() + added, if ghost_suppress { 0 } else { src_line });
        ghost_suppress = false;
    }
    if let Some(acc) = pending_struct {
        bail!("struct {} is missing its `ends`", acc.label);
    }
    if let Some(acc) = pending_string {
        bail!("string block is missing its `{}`", acc.end_kw);
    }
    Ok((out, map))
}

/// An open high-level block during lowering. `If` tracks its next clause-label
/// number and whether an `.else` has been seen; loops carry only their id (their
/// labels are derived from it). The stack lets each `.endX` match its opener and
/// `.break`/`.continue` find the innermost enclosing loop.
enum Block {
    If { id: usize, next: usize, else_seen: bool },
    While { id: usize },
    Repeat { id: usize },
    For { id: usize, reg: String },
    Forever { id: usize },
    /// `proc … endproc` — `uses` are the saved GP registers (popped in reverse);
    /// `xmm` are the non-volatile vector registers an `fproc` saves with movups.
    /// `frame_size` > 0 for a `frame` proc: the bytes reserved once in the
    /// prologue (shadow space + outgoing args) so the calls inside skip their
    /// per-call alignment; the epilogue releases it. 0 for an unframed proc.
    Proc { uses: Vec<String>, xmm: Vec<&'static str>, frame_size: usize },
}

/// A parsed `proc` header.
struct ProcHdr {
    name: String,
    uses: Vec<String>,
    ins: Vec<String>,
    outs: Vec<String>,
    /// `frame` was given: reserve the shadow space + outgoing-arg area once so the
    /// calls inside skip their per-call alignment.
    frame: bool,
    /// `fproc` (not `proc`): the float variant, whose `uses` may also list
    /// xmm6..xmm15 — the prologue saves/restores those vector registers for you.
    fp: bool,
}

/// Detect a `proc`/`fproc` opener, returning its header text and whether it's the
/// float variant. `strip_keyword` is prefix-based, so `fproc` never matches `proc`.
fn strip_proc_kw(t: &str) -> Option<(&str, bool)> {
    if let Some(r) = strip_keyword(t, "fproc") {
        return Some((r, true));
    }
    strip_keyword(t, "proc").map(|r| (r, false))
}

/// `proc NAME [uses R…] [in R…] [out R…] [frame]` — the keywords delimit
/// space/comma-separated register lists; `frame` is a bare flag.
fn parse_proc(rest: &str, fp: bool) -> ProcHdr {
    let mut h = ProcHdr {
        name: String::new(),
        uses: Vec::new(),
        ins: Vec::new(),
        outs: Vec::new(),
        frame: false,
        fp,
    };
    let mut bucket = 0u8; // 0 = name, 1 = uses, 2 = in, 3 = out
    for tok in rest.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
        match tok.to_ascii_lowercase().as_str() {
            "uses" => bucket = 1,
            "in" => bucket = 2,
            "out" => bucket = 3,
            "frame" => h.frame = true,
            _ => match bucket {
                0 => h.name = tok.to_string(),
                1 => h.uses.push(tok.to_ascii_lowercase()),
                2 => h.ins.push(tok.to_ascii_lowercase()),
                _ => h.outs.push(tok.to_ascii_lowercase()),
            },
        }
    }
    h
}

/// Split a proc's `uses` list into the GP registers (saved with push/pop) and the
/// non-volatile xmm registers (saved with movups — `fproc` only), de-duplicating
/// the latter and canonicalizing ymm/zmm to their xmm name.
fn split_uses(uses: &[String]) -> (Vec<String>, Vec<&'static str>) {
    let mut gprs = Vec::new();
    let mut xmms: Vec<&'static str> = Vec::new();
    for u in uses {
        if let Some(x) = nonvol_xmm(u) {
            if !xmms.contains(&x) {
                xmms.push(x);
            }
        } else {
            gprs.push(u.clone());
        }
    }
    (gprs, xmms)
}

/// The number of Win64 arguments a call line marshals (including the COM `this`),
/// for sizing a framed proc's outgoing-argument area. `None` if not a call.
fn call_arg_count(t: &str) -> Option<usize> {
    if let Some(rest) = strip_keyword(t, "invoke") {
        return Some(split_top_commas(rest).len().saturating_sub(1)); // minus the function
    }
    if let Some(rest) = strip_keyword(t, "comcall") {
        return Some(1 + split_top_commas(rest).len().saturating_sub(3)); // this + method args
    }
    if is_method_call_shape(t) {
        let inside = t
            .split_once('(')
            .and_then(|(_, r)| r.rsplit_once(')'))
            .map(|(a, _)| a.trim())
            .unwrap_or("");
        let argc = if inside.is_empty() { 0 } else { split_top_commas(inside).len() };
        return Some(1 + argc); // this + method args
    }
    None
}

/// The bytes a framed proc reserves: 32 (shadow) + the outgoing stack args,
/// rounded to 16. A framed proc aligns the stack itself (an `rbp` anchor +
/// `and rsp,-16`), so the size doesn't depend on the saved-register count — and
/// the proc is correct no matter how the caller entered it.
fn proc_frame_size(max_args: usize) -> usize {
    (32 + max_args.saturating_sub(4) * 8 + 15) & !15
}

/// The proc epilogue: release the frame (restore rsp from the rbp anchor),
/// restore the saved registers in reverse, then `ret`.
fn proc_epilogue(uses: &[String], xmm: &[&'static str], frame_size: usize) -> String {
    let mut s = String::new();
    if frame_size > 0 {
        s.push_str("  mov rsp, rbp\n  pop rbp\n");
    }
    // fproc: reload the saved vector registers, then release their stack area.
    for (i, r) in xmm.iter().enumerate() {
        s.push_str(&format!("  movups {r}, [rsp + {}]\n", i * 16));
    }
    if !xmm.is_empty() {
        s.push_str(&format!("  add rsp, {}\n", xmm.len() * 16));
    }
    for r in uses.iter().rev() {
        s.push_str(&format!("  pop {r}\n"));
    }
    s.push_str("  ret\n");
    s
}

/// Is there an enclosing framed proc — i.e. should a call inside use the cheap
/// form (no per-call alignment, reuse the proc's reserved frame)?
fn in_framed_proc(stack: &[Block]) -> bool {
    stack.iter().any(|b| matches!(b, Block::Proc { frame_size, .. } if *frame_size > 0))
}

impl Block {
    /// `(continue_target, break_target)` if this block is a loop. `.continue`
    /// re-evaluates the test (a `.for` still runs its increment); `.break` jumps
    /// past the loop.
    fn loop_targets(&self) -> Option<(String, String)> {
        Some(match self {
            Block::While { id } => (format!("__while{id}_top"), format!("__while{id}_end")),
            Block::Repeat { id } => (format!("__repeat{id}_test"), format!("__repeat{id}_end")),
            Block::For { id, .. } => (format!("__for{id}_cont"), format!("__for{id}_end")),
            Block::Forever { id } => (format!("__forever{id}_top"), format!("__forever{id}_end")),
            Block::If { .. } | Block::Proc { .. } => return None,
        })
    }
}

/// The `jcc` taken when a `reg <relop> val` condition is `want` (true / false).
/// Unsigned by default (`ja`/`jb`/…); an `s`-prefixed operator (`s<`, `s>=`, …)
/// picks the signed branch (`jl`/`jge`/…). Equality is sign-agnostic.
fn cond_branch(relop: &str, want: bool) -> Option<&'static str> {
    #[rustfmt::skip]
    let b = match (relop, want) {
        ("<=", true) => "jbe", ("<=", false) => "ja",
        ("<",  true) => "jb",  ("<",  false) => "jae",
        (">=", true) => "jae", (">=", false) => "jb",
        (">",  true) => "ja",  (">",  false) => "jbe",
        ("s<=", true) => "jle", ("s<=", false) => "jg",
        ("s<",  true) => "jl",  ("s<",  false) => "jge",
        ("s>=", true) => "jge", ("s>=", false) => "jl",
        ("s>",  true) => "jg",  ("s>",  false) => "jle",
        ("==", true) => "je",  ("==", false) => "jne",
        ("!=", true) => "jne", ("!=", false) => "je",
        _ => return None,
    };
    Some(b)
}

/// Split a condition into `(lhs, relop, rhs)`; longest operators first so `s<=`
/// beats `s<` beats `<`.
fn split_condition(cond: &str) -> Option<(&str, &str, &str)> {
    for op in ["s<=", "s>="] {
        if let Some(p) = cond.find(op) {
            return Some((&cond[..p], op, &cond[p + op.len()..]));
        }
    }
    for op in ["<=", ">=", "==", "!=", "s<", "s>"] {
        if let Some(p) = cond.find(op) {
            return Some((&cond[..p], op, &cond[p + op.len()..]));
        }
    }
    for op in ["<", ">"] {
        if let Some(p) = cond.find(op) {
            return Some((&cond[..p], op, &cond[p + 1..]));
        }
    }
    None
}

/// Lower `reg <relop> val` to `cmp reg, val` plus the branch to `target`, taken
/// when the condition is `want`. `want=false` skips a clause / leaves a loop
/// (`.if`/`.while`/`.until`); `want=true` is the `.break if`/`.continue if` form.
/// Operands resolve like any instruction's, so a constant or `'z'` works.
fn cond_jump(
    cond: &str,
    target: &str,
    want: bool,
    kb: &Kb,
    labels: &HashSet<String>,
) -> Result<String> {
    let (lhs, relop, rhs) = split_condition(cond)
        .ok_or_else(|| anyhow::anyhow!("condition wants `reg <relop> value`, got `{cond}`"))?;
    let branch = cond_branch(relop, want)
        .ok_or_else(|| anyhow::anyhow!("unknown relational operator `{relop}`"))?;
    let lhs = resolve_operands(lhs.trim(), kb, labels)?;
    let rhs = resolve_operands(rhs.trim(), kb, labels)?;
    Ok(format!("  cmp {lhs}, {rhs}\n  {branch} {target}\n"))
}

/// Lower `.break`/`.continue` (`brk` selects which) with an optional `if <cond>`
/// suffix: unconditional → a `jmp` to the innermost loop's end/continue target;
/// `if <cond>` → a `cmp` plus the branch taken when the condition holds.
fn loop_jump(
    rest: &str,
    brk: bool,
    stack: &[Block],
    kb: &Kb,
    labels: &HashSet<String>,
    src_line: usize,
    raw: &str,
) -> Result<String> {
    let word = if brk { "break" } else { "continue" };
    let (cont, end) = stack
        .iter()
        .rev()
        .find_map(Block::loop_targets)
        .ok_or_else(|| anyhow::anyhow!("line {src_line}: `.{word}` outside a loop"))?;
    let target = if brk { end } else { cont };
    let rest = rest.trim();
    if rest.is_empty() {
        Ok(format!("  jmp {target}\n"))
    } else if let Some(cond) = strip_keyword(rest, "if") {
        cond_jump(cond, &target, true, kb, labels)
            .with_context(|| format!("line {src_line}: `{}`", raw.trim()))
    } else {
        bail!("line {src_line}: `.{word}` expects nothing or `if <cond>`")
    }
}

/// Collect labels defined in this source so we never resolve them as constants.
fn collect_labels(src: &str) -> HashSet<String> {
    let mut labels = HashSet::new();
    for raw in src.lines() {
        let t = strip_comment(raw).trim().to_string();
        if let Some(rest) = t.strip_prefix(".globl").or_else(|| t.strip_prefix(".global")) {
            labels.insert(rest.trim().to_string());
        }
        // `name:` (optionally followed by an instruction on the same line)
        if let Some(name) = leading_label(&t) {
            labels.insert(name.to_string());
        }
        // A MASM data label (`msg BYTE …`) has no colon — collect it too.
        if let Some((Some(label), _, _)) = parse_data_line(&t) {
            labels.insert(label.to_string());
        }
    }
    labels
}

/// If `line` begins with `name:`, return `name`.
fn leading_label(line: &str) -> Option<&str> {
    let colon = line.find(':')?;
    let name = line[..colon].trim();
    if !name.is_empty() && name.chars().all(is_ident_char) && name.chars().next()?.is_ascii_alphabetic() {
        Some(name)
    } else {
        None
    }
}

/// Resolve operand identifiers on one instruction line (keeping any `label:`
/// prefix and the mnemonic verbatim).
fn rewrite_line(line: &str, kb: &Kb, labels: &HashSet<String>) -> Result<String> {
    let mut prefix = String::new();
    let mut rest = line.trim_end();
    // Peel a leading label (`name:`), keeping it.
    if let Some(name) = leading_label(rest.trim_start()) {
        let after = &rest.trim_start()[name.len()..];
        let after = after.trim_start();
        let after = after.strip_prefix(':').unwrap_or(after);
        prefix = format!("{name}: ");
        rest = after.trim_start();
        if rest.is_empty() {
            return Ok(format!("{name}:"));
        }
    }
    // Split mnemonic from operands.
    let (mnem, ops) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim_start()),
        None => (rest, ""),
    };
    if ops.is_empty() {
        return Ok(format!("{prefix}{mnem}"));
    }
    let resolved = resolve_operands(ops, kb, labels)?;
    Ok(format!("{prefix}{mnem} {resolved}"))
}

/// Rewrite identifiers in operand text: `sizeof(T)`, `Struct.field`, and bare
/// constant/enum names. Everything else is copied verbatim.
fn resolve_operands(text: &str, kb: &Kb, labels: &HashSet<String>) -> Result<String> {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        // String / char literals: copy verbatim.
        if c == '\'' || c == '"' {
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i] as char);
                let done = b[i] as char == c;
                i += 1;
                if done {
                    break;
                }
            }
            continue;
        }
        // Identifier (with optional dotted field path).
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (is_ident_char(b[i] as char) || b[i] as char == '.') {
                i += 1;
            }
            let tok = &text[start..i];
            // sizeof(T) / sizeof T
            if tok == "sizeof" {
                let (consumed, ty) = read_sizeof_arg(&text[i..]);
                i += consumed;
                match kb.sizeof(ty)? {
                    Some(n) => out.push_str(&n.to_string()),
                    None => bail!("sizeof: unknown type '{ty}'"),
                }
                continue;
            }
            out.push_str(&resolve_token(tok, kb, labels)?);
            continue;
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// After the `sizeof` keyword, consume `(Type)` or ` Type`. Returns
/// `(bytes_consumed, type_name)`.
fn read_sizeof_arg(after: &str) -> (usize, &str) {
    let trimmed_len = after.len() - after.trim_start().len();
    let s = after.trim_start();
    if let Some(inner) = s.strip_prefix('(') {
        if let Some(close) = inner.find(')') {
            let ty = inner[..close].trim();
            return (trimmed_len + 1 + close + 1, ty);
        }
    }
    // bare: read an identifier
    let end = s.find(|ch: char| !is_ident_char(ch) && ch != '.').unwrap_or(s.len());
    (trimmed_len + end, s[..end].trim())
}

/// Resolve a single identifier token: `Struct.field` → offset, bare name → value.
fn resolve_token(tok: &str, kb: &Kb, labels: &HashSet<String>) -> Result<String> {
    if let Some(dot) = tok.find('.') {
        let (lhs, field) = (&tok[..dot], &tok[dot + 1..]);
        if !is_register(lhs) && !labels.contains(lhs) {
            // `Struct.field` or nested `Struct.sub.field` -> its byte offset.
            if let Some((off, _)) = field_path(kb, lhs, field)? {
                return Ok(off.to_string());
            }
        }
        return Ok(tok.to_string());
    }
    if is_register(tok) || labels.contains(tok) {
        return Ok(tok.to_string());
    }
    if let Some(v) = kb.resolve(tok)?.first() {
        return Ok(v.i64v.to_string());
    }
    Ok(tok.to_string())
}

/// Expand `invoke Func, a0, a1, …` to the Win64 call sequence.
fn expand_invoke(rest: &str, kb: &Kb, labels: &HashSet<String>, framed: bool) -> Result<String> {
    let parts = split_top_commas(rest);
    let func = parts.first().map(|s| s.trim()).unwrap_or("");
    if func.is_empty() {
        bail!("invoke: missing function name");
    }
    let args: Vec<String> = parts[1..].iter().map(|s| s.trim().to_string()).collect();
    let n = args.len();

    // Optional sanity check against the known signature.
    if let Some(f) = kb.function(func)? {
        if f.params.len() != n {
            return Ok(format!(
                "  ; WARNING: {func} expects {} args, got {n}\n{}",
                f.params.len(),
                emit_call(func, &args, kb, labels, framed)?
            ));
        }
    }
    emit_call(func, &args, kb, labels, framed)
}

/// Marshal the Win64 arguments then `call`. Without `framed`, the call sets up
/// its own aligned frame (push rbx / and rsp,-16 / sub / restore). With `framed`,
/// the enclosing proc has already aligned the stack and reserved the shadow +
/// outgoing-arg area, so the call is just the arg moves and the `call` — the lean
/// form your insight asks for.
fn emit_call(func: &str, args: &[String], kb: &Kb, labels: &HashSet<String>, framed: bool) -> Result<String> {
    let n = args.len();
    // The function's parameter types — so a float param marshals to xmm on its own.
    let ptys: Vec<String> = kb
        .function(func)?
        .map(|f| f.params.iter().map(|p| p.type_name.clone()).collect())
        .unwrap_or_default();
    let mut o = String::new();
    o.push_str(&format!("  ; invoke {func} ({n} args)\n"));
    if !framed {
        let frame = 32 + ((n.saturating_sub(4) * 8 + 15) & !15);
        o.push_str("  push rbx\n  mov rbx, rsp\n  and rsp, -16\n");
        o.push_str(&format!("  sub rsp, {frame}\n"));
    }
    // Stack args (index >= 4) at [rsp+32+...] — the reserved outgoing-arg area.
    // A float there goes as its raw bits, so just strip the annotation.
    for (idx, arg) in args.iter().enumerate().skip(4) {
        let off = 32 + (idx - 4) * 8;
        let v = float_arg(arg).map(|(_, v)| v).unwrap_or(arg);
        o.push_str(&load_arg("rax", v, kb, labels)?);
        o.push_str(&format!("  mov [rsp + {off}], rax\n"));
    }
    // Register args (0..=3): integers → rcx/rdx/r8/r9, floats → xmm0–3.
    for (idx, arg) in args.iter().enumerate().take(4) {
        o.push_str(&marshal_reg_arg(idx, arg, ptys.get(idx).map(|s| s.as_str()), kb, labels)?);
    }
    o.push_str(&format!("  call {func}\n"));
    if !framed {
        o.push_str("  mov rsp, rbx\n  pop rbx\n");
    }
    Ok(o)
}

/// Find a COM method (with its vtable index and param types) by walking the
/// base-interface chain — so inherited `IUnknown`/parent methods resolve too.
fn find_method(kb: &Kb, iface: &str, method: &str) -> Result<Option<winkb::Method>> {
    let mut name = iface.to_string();
    for _ in 0..32 {
        let Some(i) = kb.interface(&name)? else { return Ok(None) };
        if let Some(m) = i.methods.iter().find(|m| m.name == method) {
            return Ok(Some(m.clone()));
        }
        match i.base {
            // The base is a fully-qualified name; the lookup wants the simple name.
            Some(b) => name = b.rsplit('.').next().unwrap_or(&b).to_string(),
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// The vtable index of a COM method (or `None` if unknown).
fn vtable_index_of(kb: &Kb, iface: &str, method: &str) -> Result<Option<i64>> {
    Ok(find_method(kb, iface, method)?.map(|m| m.vtable_index))
}

/// A floating-point parameter type → `Some(is_double)`; otherwise `None`. Such an
/// argument rides an xmm register, not an integer one. (A pointer like `f32*` is a
/// pointer, not a float — only the bare scalar types match.)
fn float_type(t: &str) -> Option<bool> {
    match t.trim() {
        "f32" | "FLOAT" | "float" | "single" => Some(false),
        "f64" | "double" => Some(true),
        _ => None,
    }
}

/// Expand `comcall obj, Interface, Method[, args…]` to a COM vtable call. `obj`
/// is the interface pointer (the `this`, arg 0); the method's slot is looked up
/// in the knowledge db. The expansion is plain, visible code — same Win64
/// marshaling as `invoke`, but the call is indirect through the vtable.
fn expand_comcall(rest: &str, kb: &Kb, labels: &HashSet<String>, framed: bool) -> Result<String> {
    let parts = split_top_commas(rest);
    if parts.len() < 3 {
        bail!("comcall needs at least: object, interface, method");
    }
    let iface = parts[1].trim();
    let method = parts[2].trim();
    let m = find_method(kb, iface, method)?
        .ok_or_else(|| anyhow::anyhow!("comcall: interface '{iface}' has no method '{method}'"))?;
    let idx = m.vtable_index;

    // The object is the `this` pointer (arg 0); the rest are the method's args.
    let mut all: Vec<String> = Vec::with_capacity(parts.len() - 1);
    all.push(parts[0].trim().to_string());
    all.extend(parts[3..].iter().map(|s| s.trim().to_string()));

    let n = all.len();
    let disp = idx * 8;

    let mut o = String::new();
    o.push_str(&format!("  ; comcall {iface}::{method}  (vtbl[{idx}], this + {} arg(s))\n", n - 1));
    if !framed {
        let frame = 32 + ((n.saturating_sub(4) * 8 + 15) & !15);
        o.push_str("  push rbx\n  mov rbx, rsp\n  and rsp, -16\n");
        o.push_str(&format!("  sub rsp, {frame}\n"));
    }
    for (i, arg) in all.iter().enumerate().skip(4) {
        let off = 32 + (i - 4) * 8;
        let v = float_arg(arg).map(|(_, v)| v).unwrap_or(arg);
        o.push_str(&load_arg("rax", v, kb, labels)?);
        o.push_str(&format!("  mov [rsp + {off}], rax\n"));
    }
    // Marshal the method's register args (rdx/r8/r9) BEFORE loading `this` into rcx,
    // so an argument that lives in rcx/ecx (a count or index computed there) survives
    // instead of being clobbered by the `this` load. (help.md trap #9 — handled by the
    // macro now, not just a documented footgun.)
    for (i, arg) in all.iter().enumerate().take(4).skip(1) {
        let pty = m.params.get(i - 1).map(|s| s.as_str());
        o.push_str(&marshal_reg_arg(i, arg, pty, kb, labels)?);
    }
    // `this` (arg 0) → rcx, LAST — after every method arg is already in place.
    o.push_str(&marshal_reg_arg(0, &all[0], None, kb, labels)?);
    o.push_str("  mov rax, [rcx]\n"); // vtable, from the `this` pointer in rcx
    o.push_str(&format!("  call qword ptr [rax + {disp}]\n"));
    if !framed {
        o.push_str("  mov rsp, rbx\n  pop rbx\n");
    }
    Ok(o)
}

/// The 16 bytes of a COM GUID, in the on-the-wire mixed-endian layout
/// (Data1 u32 LE, Data2 u16 LE, Data3 u16 LE, Data4 8 bytes as written).
fn guid_to_bytes(g: &str) -> Result<[u8; 16]> {
    let hex: String = g.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        bail!("malformed GUID '{g}' (need 32 hex digits)");
    }
    let b = |i: usize| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    Ok([
        b(3), b(2), b(1), b(0), // Data1 (u32 LE)
        b(5), b(4), // Data2 (u16 LE)
        b(7), b(6), // Data3 (u16 LE)
        b(8), b(9), b(10), b(11), b(12), b(13), b(14), b(15), // Data4 (as written)
    ])
}

/// Expand `iid Interface` to the 16 GUID bytes of that interface's IID, ready to
/// point `CoCreateInstance`/`QueryInterface` at.
fn emit_iid(rest: &str, kb: &Kb) -> Result<String> {
    let name = rest.trim();
    let i = kb
        .interface(name)?
        .ok_or_else(|| anyhow::anyhow!("iid: unknown interface '{name}'"))?;
    let guid = i.iid.ok_or_else(|| anyhow::anyhow!("iid: {name} has no IID"))?;
    let b = guid_to_bytes(&guid)?;
    // Emit the GUID struct: Data1 (u32), Data2/Data3 (u16), Data4 (8 bytes). rasm's
    // .long/.word are little-endian, which is exactly the COM in-memory layout.
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    let mut o = format!("  ; IID of {name} = {{{guid}}}\n");
    o.push_str(&format!("  .long 0x{d1:08x}\n  .word 0x{d2:04x}\n  .word 0x{d3:04x}\n"));
    for byte in &b[8..16] {
        o.push_str(&format!("  .byte 0x{byte:02x}\n"));
    }
    Ok(o)
}

/// An accumulating `LABEL struct TYPE … ends` data block.
struct StructAccum {
    label: String,
    ty: String,
    fields: Vec<(String, String)>, // (field path, value)
}

/// A `.ASCIISTRING … .ENDASCIISTRING` (or `.WIDESTRING … .ENDWIDESTRING`) raw
/// text block: every line between the directives, captured verbatim.
struct StringAccum {
    lines: Vec<String>,
    wide: bool,
    end_kw: &'static str,
}

/// Lower a raw text block to data directives. Each captured line becomes its
/// bytes plus the line-ending newline (so the block is the text *as written*,
/// including the line breaks). ASCII → one `.ascii "…\n"` per line (rasm decodes
/// the `\n`, `\"`, `\\` escapes); wide → UTF-16LE code units via `.word`.
fn emit_string_block(acc: &StringAccum) -> String {
    let mut out = String::new();
    for line in &acc.lines {
        if acc.wide {
            let mut units: Vec<String> = line.encode_utf16().map(|u| u.to_string()).collect();
            units.push("10".to_string()); // the line-ending newline
            out.push_str(&format!("  .word {}\n", units.join(", ")));
        } else {
            let esc = line.replace('\\', "\\\\").replace('"', "\\\"");
            out.push_str(&format!("  .ascii \"{esc}\\n\"\n"));
        }
    }
    out
}

/// `LABEL struct TYPE` — the opener of a struct-instance data block.
fn parse_struct_open(t: &str) -> Option<(String, String)> {
    let toks: Vec<&str> = t.split_whitespace().collect();
    (toks.len() == 3 && toks[1] == "struct").then(|| (toks[0].to_string(), toks[2].to_string()))
}

/// Byte size of a field from its db type name: primitives mapped directly,
/// pointers are 8, otherwise the db `sizeof` (unknown enums fall back to 4).
fn field_size(kb: &Kb, ty: &str) -> usize {
    let t = ty.trim();
    if t.ends_with('*') {
        return 8;
    }
    match t {
        "u8" | "i8" | "BYTE" | "byte" | "CHAR" | "BOOLEAN" => 1,
        "u16" | "i16" | "WORD" | "WCHAR" | "SHORT" | "USHORT" => 2,
        "u32" | "i32" | "DWORD" | "BOOL" | "LONG" | "ULONG" | "UINT" | "INT" | "FLOAT" | "f32" => 4,
        "u64" | "i64" | "QWORD" | "f64" | "DOUBLE" | "HWND" | "HANDLE" | "HINSTANCE" | "HDC"
        | "HMODULE" | "HMENU" | "HBRUSH" | "HICON" | "HCURSOR" | "LPARAM" | "WPARAM" | "LRESULT"
        | "SIZE_T" | "INT_PTR" | "UINT_PTR" | "PVOID" | "LPVOID" | "PWSTR" | "PCWSTR" => 8,
        _ => kb.sizeof(t).ok().flatten().map(|s| s as usize).unwrap_or(4),
    }
}

/// Resolve a (possibly nested) field path like `BufferDesc.Width` within `root`
/// to its byte offset and size, descending through nested struct types.
fn field_path(kb: &Kb, root: &str, path: &str) -> Result<Option<(i64, usize)>> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = root.to_string();
    let mut off = 0i64;
    for (i, part) in parts.iter().enumerate() {
        let Some(layout) = kb.layout(&cur)? else { return Ok(None) };
        let Some(f) = layout.fields.iter().find(|f| f.name == *part) else { return Ok(None) };
        off += f.offset;
        if i == parts.len() - 1 {
            return Ok(Some((off, field_size(kb, &f.type_name))));
        }
        // Descend into the field's (possibly fully-qualified) struct type.
        cur = f.type_name.rsplit('.').next().unwrap_or(&f.type_name).to_string();
    }
    Ok(None)
}

/// Pre-scan `comobj NAME : Interface` declarations into a `name -> interface`
/// map, so a later `NAME.Method(...)` call knows the object's interface type.
fn collect_com_objs(src: &str) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for raw in src.lines() {
        let t = strip_comment(raw).trim();
        if let Some(rest) = strip_keyword(t, "comobj") {
            if let Some((name, iface)) = rest.split_once(':') {
                m.insert(name.trim().to_string(), iface.trim().to_string());
            }
        }
    }
    m
}

/// The `comobj NAME : Interface` bindings declared in `src`, as sorted
/// `(name, interface)` pairs — for the IDE to resolve a typed pointer.
pub fn com_bindings(src: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = collect_com_objs(src).into_iter().collect();
    v.sort();
    v
}

/// Recognise a typed COM method call `obj.Method(args)` where `obj` is a declared
/// `comobj`. Returns `(obj, interface, method, args)`.
fn parse_method_call(
    t: &str,
    com_objs: &std::collections::HashMap<String, String>,
) -> Option<(String, String, String, String)> {
    let (lhs, rest) = t.split_once('.')?;
    let name = lhs.trim();
    let iface = com_objs.get(name)?;
    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    let method = rest[..open].trim();
    if method.is_empty() || !method.chars().all(is_ident_char) {
        return None;
    }
    Some((name.to_string(), iface.clone(), method.to_string(), rest[open + 1..close].trim().to_string()))
}

/// Lay out a struct-instance data block: a label, then each constant field at its
/// db-resolved offset (zero-filled between), as visible `.long`/`.word`/`.byte`.
fn emit_struct(kb: &Kb, acc: &StructAccum) -> Result<String> {
    let size = kb
        .sizeof(&acc.ty)?
        .ok_or_else(|| anyhow::anyhow!("struct: unknown type '{}'", acc.ty))? as usize;
    let mut entries: Vec<(usize, usize, String, String)> = Vec::new();
    for (path, val) in &acc.fields {
        let (o, fs) = field_path(kb, &acc.ty, path)?
            .ok_or_else(|| anyhow::anyhow!("{} has no field '{path}'", acc.ty))?;
        entries.push((o as usize, fs, val.clone(), path.clone()));
    }
    entries.sort_by_key(|e| e.0);

    let mut o = format!("{}:\n  ; {} ({size} bytes)\n", acc.label, acc.ty);
    let mut pos = 0usize;
    for (off, fs, val, name) in &entries {
        if *off < pos {
            bail!("struct {}: field '{name}' overlaps an earlier field", acc.label);
        }
        if *off > pos {
            o.push_str(&format!("  .zero {}\n", off - pos));
        }
        let dir = match fs {
            1 => "byte",
            2 => "word",
            8 => "quad",
            _ => "long",
        };
        o.push_str(&format!("  .{dir} {val}    ; {name} @ {off}\n"));
        pos = off + fs;
    }
    if pos < size {
        o.push_str(&format!("  .zero {}\n", size - pos));
    }
    Ok(o)
}

/// Emit the instruction(s) to load `arg` into `reg`. A bare label/symbol becomes
/// its address (`lea`); a number/register/memory becomes a `mov`.
fn load_arg(reg: &str, arg: &str, kb: &Kb, labels: &HashSet<String>) -> Result<String> {
    let resolved = resolve_operands(arg, kb, labels)?;
    let r = resolved.trim();
    if r.starts_with('[') {
        return Ok(format!("  mov {reg}, {r}\n"));
    }
    // A pure identifier left unresolved → a label/extern → take its address.
    if r.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && r.chars().all(|c| is_ident_char(c))
        && !is_register(r)
    {
        return Ok(format!("  lea {reg}, [rip + {r}]\n"));
    }
    Ok(format!("  mov {reg}, {r}\n"))
}

/// A float argument annotated `real4 X` / `f32 X` (or `real8`/`f64`): the value
/// goes in an xmm register, not an integer one. Returns (is-double, the value).
/// (winkb has no COM param types, so floats are marked explicitly — and visibly.)
fn float_arg(arg: &str) -> Option<(bool, &str)> {
    let a = arg.trim();
    for (kw, wide) in [("real4", false), ("f32", false), ("real8", true), ("f64", true)] {
        if let Some(rest) = a.strip_prefix(kw) {
            if rest.starts_with(char::is_whitespace) {
                return Some((wide, rest.trim_start()));
            }
        }
    }
    None
}

/// Marshal argument `idx` (0..3) into its slot. A float — recognised either from
/// the db param type (`param_ty`) or an explicit `real4`/`real8` annotation — goes
/// via movss/movsd into xmm{idx}; anything else into its integer register.
fn marshal_reg_arg(
    idx: usize,
    arg: &str,
    param_ty: Option<&str>,
    kb: &Kb,
    labels: &HashSet<String>,
) -> Result<String> {
    // explicit annotation wins (a fallback when the db is wrong); else the type.
    let float = float_arg(arg)
        .map(|(w, v)| (w, v.to_string()))
        .or_else(|| param_ty.and_then(float_type).map(|w| (w, arg.to_string())));
    if let Some((wide, val)) = float {
        let ins = if wide { "movsd" } else { "movss" };
        let rv = resolve_operands(&val, kb, labels)?;
        return Ok(format!("  {ins} xmm{idx}, {}\n", rv.trim()));
    }
    load_arg(ARG_REGS[idx], arg, kb, labels)
}

// ── small helpers ────────────────────────────────────────────────────────────

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn strip_comment(line: &str) -> &str {
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (i, c) in line.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c), // a `;`/`#` inside a string isn't a comment
            '[' => depth += 1,
            ']' => depth -= 1,
            ';' | '#' if depth == 0 => return &line[..i],
            _ => {}
        }
    }
    line
}

/// If `t` starts with the whole word `kw`, return the remainder (trimmed).
fn strip_keyword<'a>(t: &'a str, kw: &str) -> Option<&'a str> {
    let rest = t.strip_prefix(kw)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// Split on top-level commas (commas inside `[]`, `()`, or quotes stay).
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut quote = None::<char>;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].to_string());
    out
}

fn is_register(s: &str) -> bool {
    const GPR: [&str; 16] = [
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ];
    let l = s.to_ascii_lowercase();
    if GPR.contains(&l.as_str()) || l == "rip" {
        return true;
    }
    // e/.. 32-bit, .w/.b suffixed, xmm/ymm/zmm
    for p in ["xmm", "ymm", "zmm"] {
        if let Some(n) = l.strip_prefix(p) {
            if n.parse::<u8>().is_ok() {
                return true;
            }
        }
    }
    matches!(
        l.as_str(),
        "eax" | "ebx" | "ecx" | "edx" | "esi" | "edi" | "ebp" | "esp"
            | "r8d" | "r9d" | "r10d" | "r11d" | "r12d" | "r13d" | "r14d" | "r15d"
            | "ax" | "bx" | "cx" | "dx" | "si" | "di" | "al" | "bl" | "cl" | "dl"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_scopes_private_by_capitalisation() {
        // Two module regions in ONE file; same lowercase helper name in each.
        let text = "module Alpha\nRun:\nagain:\n  jmp again\nendmodule\n\
                    module Beta\nRun2:\nagain:\n  jmp again\nendmodule\n";
        let lf = vec![0u32; text.lines().count()];
        let out = scope_modules(text, &lf);
        assert!(out.contains("Alpha$again:") && out.contains("Beta$again:"), "{out}");
        assert!(out.contains("jmp Alpha$again") && out.contains("jmp Beta$again"), "{out}");
        assert!(out.contains("Run:") && out.contains("Run2:"), "{out}"); // capitals exported
    }

    #[test]
    fn include_does_not_absorb_into_includer_module() {
        // Stream as textual expansion would inline it: file 0 = Brix, with an
        // `.include`d file 1 that declares its own module Abc, plus a marker-less
        // file 2 also nested inside Brix's region.
        let text = "module Brix\nStart:\nhelper:\n\
                    module Abc\nhelper:\n  ret\nendmodule\n\
                    glob:\n\
                    loc:\nendmodule\n";
        let lf = vec![0, 0, 0, 1, 1, 1, 1, 2, 0, 0];
        let out = scope_modules(text, &lf);
        // Brix's and Abc's `helper` are distinct; neither absorbs the other.
        assert!(out.contains("Brix$helper:") && out.contains("Abc$helper:"), "{out}");
        // The marker-less included file (id 2) is NOT pulled into Brix.
        assert!(out.contains("\nglob:") && !out.contains("Brix$glob"), "{out}");
        // Brix's own later label is still scoped.
        assert!(out.contains("Brix$loc:"), "{out}");
    }

    #[test]
    fn module_spans_files_globl_and_passthrough() {
        // Same module declared by two files; a private DATA label defined in one
        // file resolves where referenced in the other (pooled by name).
        let span = "module Canvas\nbuf BYTE 0\nendmodule\n\
                    module Canvas\nUse:\n  lea rax, [rip + buf]\nendmodule\n";
        let lf = vec![0, 0, 0, 1, 1, 1, 1];
        let so = scope_modules(span, &lf);
        assert!(so.contains("Canvas$buf BYTE 0"), "{so}");
        assert!(so.contains("[rip + Canvas$buf]"), "{so}");
        // `.globl` pins a lowercase name global inside a module.
        let g = "module M\n.globl shared\nshared:\n  jmp shared\nendmodule\n";
        let gf = vec![0u32; g.lines().count()];
        let go = scope_modules(g, &gf);
        assert!(go.contains("shared:") && !go.contains("M$shared"), "{go}");
        // No module markers ⇒ byte-for-byte passthrough.
        let plain = "foo:\n  jmp foo\nBar:\n  ret\n";
        let pf = vec![0u32; plain.lines().count()];
        assert_eq!(scope_modules(plain, &pf), plain);
    }

    #[test]
    fn proc_internal_labels_are_proc_private() {
        // Two procs in ONE module, each with a lowercase `done:` jump target.
        let text = "module M\nproc Aaa\ndone:\n  jmp done\nendproc\n\
                    proc Bbb\ndone:\n  jmp done\nendproc\nendmodule\n";
        let lf = vec![0u32; text.lines().count()];
        let out = scope_modules(text, &lf);
        assert!(out.contains("Aaa$done:") && out.contains("jmp Aaa$done"), "{out}");
        assert!(out.contains("Bbb$done:") && out.contains("jmp Bbb$done"), "{out}");

        // A proc sees its module's labels (module-scoped), but its own jump targets are
        // proc-scoped.
        let t2 = "module M\nbuf BYTE 0\nproc Aaa\n  lea rax, [rip + buf]\nloop:\n  jmp loop\nendproc\nendmodule\n";
        let lf2 = vec![0u32; t2.lines().count()];
        let o2 = scope_modules(t2, &lf2);
        assert!(o2.contains("M$buf BYTE 0"), "{o2}"); // module level -> M$
        assert!(o2.contains("[rip + M$buf]"), "{o2}"); // proc references the module label
        assert!(o2.contains("Aaa$loop:") && o2.contains("jmp Aaa$loop"), "{o2}"); // proc-internal
    }

    #[test]
    fn split_commas_respects_brackets() {
        let p = split_top_commas("Func, [rsp + 8], 0x10, msg");
        assert_eq!(p, vec!["Func", " [rsp + 8]", " 0x10", " msg"]);
    }

    #[test]
    fn proc_parses_and_contract_checks() {
        let h = parse_proc("add3 uses rbx rsi in rcx rdx out rax", false);
        assert_eq!(h.name, "add3");
        assert_eq!(h.uses, ["rbx", "rsi"]);
        assert_eq!(h.ins, ["rcx", "rdx"]);
        assert_eq!(h.outs, ["rax"]);
        assert!(!h.frame);
        assert!(parse_proc("f uses rbx frame", false).frame);

        // `uses` check: modifies a callee-saved register it didn't declare
        let bad = ".code\nproc f uses rbx in rcx\n  mov rsi, rcx\nendproc\n";
        let d = proc_contract_diags(bad);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("rsi") && d[0].line == 3, "{d:?}");

        // declared → clean; volatile scratch (rcx written, r10 zeroed) is free
        let ok = ".code\nproc f uses rbx rsi in rcx rdx\n  mov rsi, rcx\n  mov rbx, rdx\n  mov rcx, 1\n  xor r10, r10\nendproc\n";
        assert!(proc_contract_diags(ok).is_empty(), "{:?}", proc_contract_diags(ok));
    }

    #[test]
    fn proc_in_out_and_frame_balance_checks() {
        // `in`: reads rcx but never sets it and didn't declare it
        let undecl = ".code\nproc f uses rbx\n  mov rbx, rcx\nendproc\n";
        let d = proc_contract_diags(undecl);
        assert!(d.iter().any(|x| x.message.contains("reads `rcx`") && x.message.contains("in rcx")), "{d:?}");
        // declaring it clears the warning
        assert!(proc_contract_diags(".code\nproc f uses rbx in rcx\n  mov rbx, rcx\nendproc\n")
            .iter().all(|x| !x.message.contains("reads")));

        // `out`: declared but never written
        let unset = ".code\nproc f out rax\n  nop\nendproc\n";
        assert!(proc_contract_diags(unset).iter().any(|x| x.message.contains("out rax") && x.message.contains("never sets")), "");

        // frame balance: a stray push before a call breaks the alignment
        let stray = ".code\nproc f frame\n  push rcx\n  invoke Sleep, 0\nendproc\n";
        assert!(proc_contract_diags(stray).iter().any(|x| x.message.contains("off the frame level")), "");
        // balanced push/pop around no call → fine
        let balanced = ".code\nproc f frame\n  push rcx\n  pop rcx\n  invoke Sleep, 0\nendproc\n";
        assert!(proc_contract_diags(balanced).iter().all(|x| !x.message.contains("frame level")), "{:?}", proc_contract_diags(balanced));
    }

    #[test]
    fn framed_proc_sizing_and_lean_calls() {
        // arg counts (incl. the COM `this`)
        assert_eq!(call_arg_count("invoke Foo, a, b"), Some(2));
        assert_eq!(call_arg_count("comcall obj, I, M, a, b"), Some(3));
        assert_eq!(call_arg_count("p.Draw(3, 0)"), Some(3));
        assert_eq!(call_arg_count("mov rax, 1"), None);
        // frame size: 32 shadow + outgoing stack args, rounded to 16 (the proc
        // aligns itself, so size doesn't depend on the saved-register count).
        assert_eq!(proc_frame_size(2), 32); // ≤4 args → shadow only
        assert_eq!(proc_frame_size(4), 32);
        assert_eq!(proc_frame_size(5), 48); // 1 stack arg
        assert_eq!(proc_frame_size(7), 64); // 3 stack args

        // A framed proc aligns once (rbp anchor); the call inside drops its own
        // ceremony and must not re-align.
        let Some(kb) = kb() else { return };
        let low = lower(".code\nproc w uses rbx frame\n  invoke Sleep, 0\nendproc\n", &kb).unwrap();
        assert!(low.contains("and rsp, -16") && low.contains("mov rsp, rbp"), "robust frame:\n{low}");
        let body = low.split("sub rsp, 32").nth(1).unwrap();
        assert!(!body.contains("and rsp, -16"), "the lean call must not re-align:\n{body}");
        assert!(body.contains("call Sleep"));
    }

    #[test]
    fn fproc_saves_xmm_and_checks_clobbers() {
        let Some(kb) = kb() else { return };
        // codegen: an fproc reserves a movups-saved area for its xmm `uses` and
        // reloads it in the epilogue, wrapping any frame setup.
        let low = lower(
            ".code\nfproc m uses xmm6 xmm7 frame\n  mulps xmm6, xmm7\n  invoke Sleep, 0\nendproc\n",
            &kb,
        )
        .unwrap();
        assert!(low.contains("sub rsp, 32"), "reserve 2*16 xmm area:\n{low}");
        assert!(
            low.contains("movups [rsp + 0], xmm6") && low.contains("movups [rsp + 16], xmm7"),
            "save both xmm:\n{low}"
        );
        assert!(
            low.contains("movups xmm6, [rsp + 0]") && low.contains("add rsp, 32"),
            "restore + release:\n{low}"
        );

        // an unframed fproc still saves (movups is alignment-free)
        let low2 = lower(".code\nfproc f uses xmm8\n  addps xmm8, xmm8\nendproc\n", &kb).unwrap();
        assert!(low2.contains("sub rsp, 16") && low2.contains("movups [rsp + 0], xmm8"), "leaf save:\n{low2}");

        // ymm/zmm in `uses` canonicalize to the 128-bit xmm save
        let low3 = lower(".code\nfproc g uses ymm6\n  vmovaps ymm6, ymm0\nendproc\n", &kb).unwrap();
        assert!(low3.contains("movups [rsp + 0], xmm6"), "ymm6 -> xmm6 save:\n{low3}");

        // a plain proc may not list xmm in `uses` (it can only push GP registers)
        assert!(
            lower(".code\nproc p uses xmm6\n  ret\nendproc\n", &kb).is_err(),
            "plain proc + xmm in uses must error"
        );

        // checker: an fproc's xmm `uses` are preserved → clobbering them is clean
        assert!(proc_contract_diags(".code\nfproc m uses xmm6\n  mulps xmm6, xmm6\nendproc\n").is_empty());
        // …but clobbering an xmm NOT in `uses` is an error (add it to `uses`)
        let d = proc_contract_diags(".code\nfproc m uses xmm6\n  movups xmm8, [rcx]\n  addps xmm0, xmm8\nendproc\n");
        assert!(
            d.iter().any(|x| x.message.contains("xmm8") && x.severity == Severity::Error),
            "fproc clobber-not-in-uses: {d:?}"
        );

        // checker: a plain proc clobbering a non-volatile xmm warns and points at fproc
        let w = proc_contract_diags(".code\nproc p\n  movups xmm6, [rcx]\n  addps xmm0, xmm6\nendproc\n");
        assert!(
            w.iter().any(|x| x.message.contains("xmm6") && x.message.contains("fproc") && x.severity == Severity::Warn),
            "plain-proc xmm warning: {w:?}"
        );

        // a hand-rolled movups save clears the warning (no false positive)
        assert!(
            proc_contract_diags(".code\nproc p\n  movups [rsp - 16], xmm6\n  mulps xmm6, xmm6\n  movups xmm6, [rsp - 16]\nendproc\n")
                .iter()
                .all(|x| !x.message.contains("xmm6")),
            "hand-saved xmm6 must not warn"
        );
    }

    #[test]
    fn float_args_marshal_to_xmm() {
        assert_eq!(float_arg("real4 [rip + x]"), Some((false, "[rip + x]")));
        assert_eq!(float_arg("f64 rax"), Some((true, "rax")));
        assert_eq!(float_arg("[rip + x]"), None);
        let Some(kb) = kb() else { return };
        // a real4 register-position arg → movss into that position's xmm
        let low = lower(".data\nf DWORD 0\n.code\n.globl m\nm:\n  invoke Sleep, real4 [rip + f]\n", &kb).unwrap();
        assert!(low.contains("movss xmm0, [rip + f]"), "float → xmm0:\n{low}");

        // and auto-detected from the db: ID2D1RenderTarget::DrawEllipse's strokeWidth
        // is `f32`, so a plain arg in slot 3 marshals to xmm3 with no annotation.
        let src = ".data\ncomobj p : ID2D1HwndRenderTarget\nb QWORD ?\nw DWORD 0\ne struct D2D1_ELLIPSE\nends\n.code\n.globl m\nm:\n  p.DrawEllipse(e, [rip + b], [rip + w], 0)\n";
        let dl = lower(src, &kb).unwrap();
        assert!(dl.contains("movss xmm3, [rip + w]"), "auto float→xmm3:\n{dl}");
    }

    #[test]
    fn clobber_check_catches_the_bug_and_spares_correct_code() {
        // BUG: stash a pointer in rcx, call (clobbers rcx), then dereference rcx.
        let bug = ".code\nf:\n  lea rcx, [rip + buf]\n  invoke GetTickCount\n  mov rax, [rcx]\n  ret\n";
        let d = clobber_diags(bug);
        assert_eq!(d.len(), 1, "exactly one warning: {d:?}");
        assert!(d[0].line == 5 && d[0].message.contains("rcx"), "{d:?}");

        // Correct patterns — must stay silent:
        let cases = [
            // saved in a non-volatile register
            ".code\nf:\n  lea rbx, [rip + b]\n  invoke GetTickCount\n  mov rax, [rbx]\n  ret\n",
            // using the return value (rax) after a call is the idiom
            ".code\nf:\n  invoke GetTickCount\n  mov rsi, rax\n  ret\n",
            // reloaded before use
            ".code\nf:\n  invoke GetTickCount\n  lea rcx, [rip + b]\n  mov rax, [rcx]\n  ret\n",
            // the zeroing idiom is a write, not a read
            ".code\nf:\n  invoke GetTickCount\n  xor rcx, rcx\n  mov [rcx], al\n  ret\n",
            // cdq writes rdx, so reading edx after is fine
            ".code\nf:\n  invoke GetTickCount\n  mov eax, 7\n  cdq\n  xor eax, edx\n  ret\n",
            // a plain call to a local function isn't assumed to clobber
            ".code\nf:\n  mov rcx, 5\n  call helper\n  mov [rcx], al\n  ret\n",
            // a proc's trailing invoke must NOT taint the NEXT proc's argument
            // reads — `proc`/`endproc` is a fresh function (its `in` regs are live)
            "proc a frame\n  invoke ReleaseDC, rcx, rdx\nendproc\nproc b in rcx rdx\n  mov rsi, rcx\n  mov rdi, rdx\nendproc\n",
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(clobber_diags(c).is_empty(), "case {i} should be clean: {:?}", clobber_diags(c));
        }
    }

    #[test]
    fn guid_to_bytes_is_mixed_endian() {
        // IDXGISwapChain's IID: Data1/2/3 little-endian, Data4 as written.
        let b = guid_to_bytes("310d36a0-d2e7-4c0a-aa04-6a9d23b8886a").unwrap();
        assert_eq!(
            b,
            [
                0xa0, 0x36, 0x0d, 0x31, // Data1 LE
                0xe7, 0xd2, // Data2 LE
                0x0a, 0x4c, // Data3 LE
                0xaa, 0x04, 0x6a, 0x9d, 0x23, 0xb8, 0x88, 0x6a, // Data4
            ]
        );
        assert!(guid_to_bytes("not-a-guid").is_err());
    }

    #[test]
    fn ascii_string_block_is_verbatim_text_plus_newlines() {
        let acc = StringAccum {
            lines: vec!["float4 c0;".to_string(), "return c0;".to_string()],
            wide: false,
            end_kw: ".endasciistring",
        };
        assert_eq!(
            emit_string_block(&acc),
            "  .ascii \"float4 c0;\\n\"\n  .ascii \"return c0;\\n\"\n"
        );
    }

    #[test]
    fn ascii_string_block_escapes_quotes_and_backslashes() {
        // raw line  a"b\c  →  a \" b \\ c  inside the rasm `.ascii` literal.
        let acc = StringAccum {
            lines: vec!["a\"b\\c".to_string()],
            wide: false,
            end_kw: ".endasciistring",
        };
        assert_eq!(emit_string_block(&acc), "  .ascii \"a\\\"b\\\\c\\n\"\n");
    }

    #[test]
    fn wide_string_block_emits_utf16le_words() {
        let acc = StringAccum {
            lines: vec!["Hi".to_string()],
            wide: true,
            end_kw: ".endwidestring",
        };
        // 'H'=72, 'i'=105, then the line-ending newline 10.
        assert_eq!(emit_string_block(&acc), "  .word 72, 105, 10\n");
    }

    #[test]
    fn sizeof_arg_parsing() {
        assert_eq!(read_sizeof_arg("(RECT) , rax").1, "RECT");
        assert_eq!(read_sizeof_arg(" POINT)").1, "POINT");
    }

    #[test]
    fn registers_recognized() {
        assert!(is_register("rcx") && is_register("eax") && is_register("xmm3"));
        assert!(!is_register("MB_OK") && !is_register("main"));
    }

    #[test]
    fn split_line_tag_peels_the_prefix() {
        let (n, rest) = split_line_tag("line 7: encode `xyzzy rax`: unknown mnemonic");
        assert_eq!(n, Some(7));
        assert_eq!(rest, "encode `xyzzy rax`: unknown mnemonic");
        // No tag → message returned verbatim, no line.
        assert_eq!(split_line_tag("boom"), (None, "boom".to_string()));
        assert_eq!(split_line_tag("line 7"), (None, "line 7".to_string()));
        assert_eq!(split_line_tag("line x: y"), (None, "line x: y".to_string()));
    }

    /// Open the knowledge db, or skip the test if it isn't present here.
    fn kb() -> Option<Kb> {
        let db = std::env::var("WINKB_DB")
            .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
        Kb::open(&db).ok()
    }

    #[test]
    fn comcall_resolves_via_base_chain() {
        let Some(kb) = kb() else { return };
        // Present is the interface's own slot (8); Release is inherited from
        // IUnknown (2), found by walking the base chain.
        assert_eq!(vtable_index_of(&kb, "IDXGISwapChain", "Present").unwrap(), Some(8));
        assert_eq!(vtable_index_of(&kb, "IDXGISwapChain", "Release").unwrap(), Some(2));
        let low = lower("main:\n  comcall [rip+p], IDXGISwapChain, Present, 1, 0\n", &kb).unwrap();
        assert!(low.contains("mov rax, [rcx]"), "loads vtable: {low}");
        assert!(low.contains("call qword ptr [rax + 64]"), "calls vtbl[8]: {low}");
    }

    #[test]
    fn struct_instance_lays_out_nested_fields_at_db_offsets() {
        let Some(kb) = kb() else { return };
        let src = "scd struct DXGI_SWAP_CHAIN_DESC\n  BufferDesc.Format = 87\n  BufferCount = 2\nends\n";
        let low = lower(src, &kb).unwrap();
        assert!(low.contains("scd:"), "labelled: {low}");
        assert!(low.contains("BufferDesc.Format @ 16"), "nested offset resolved: {low}");
        assert!(low.contains("BufferCount @ 40"), "top-level offset: {low}");
    }

    #[test]
    fn real_data_literals_emit_ieee_bits() {
        let Some(kb) = kb() else { return };
        let low =
            lower(".data\nx real8 2.5\ny real4 1.5\nz real8 -0.5\n.code\n.globl m\nm:\n  ret\n", &kb)
                .unwrap();
        assert!(low.contains(".quad 0x4004000000000000"), "real8 2.5 → f64 bits:\n{low}");
        assert!(low.contains(".long 0x3FC00000"), "real4 1.5 → f32 bits:\n{low}");
        assert!(low.contains(".quad 0xBFE0000000000000"), "real8 -0.5 → f64 bits:\n{low}");
    }

    #[test]
    fn typed_pointer_method_call_desugars_to_comcall() {
        let Some(kb) = kb() else { return };
        let src = "comobj pSwap : IDXGISwapChain\nmain:\n  pSwap.Present(1, 0)\n  pSwap.Release()\n";
        let low = lower(src, &kb).unwrap();
        assert!(low.contains("pSwap:") && low.contains(".zero 8"), "pointer slot: {low}");
        assert!(low.contains("mov rcx, [rip + pSwap]"), "loads the pointer: {low}");
        assert!(low.contains("call qword ptr [rax + 64]"), "Present is vtbl[8]: {low}");
        assert!(low.contains("call qword ptr [rax + 16]"), "Release is vtbl[2]: {low}");
        // a bad method on a typed pointer is an editor diagnostic
        let diags = check("comobj p : IDXGISwapChain\np.Nope()\n", &kb);
        assert!(diags.iter().any(|d| d.message.contains("no method 'Nope'")), "{diags:?}");
    }

    #[test]
    fn comcall_marshals_this_into_rcx_last() {
        let Some(kb) = kb() else { return };
        // Present(SyncInterval, Flags): the args go to rdx/r8, `this` to rcx. The `this`
        // load must come AFTER the arg loads, so an argument living in rcx/ecx (a count
        // computed there) isn't clobbered — `pObj.Method(4, ecx, 0, 0)` keeps ecx.
        // (help.md trap #9.)
        let low =
            lower("comobj pSwap : IDXGISwapChain\nmain:\n  pSwap.Present(1, 0)\n", &kb).unwrap();
        let this_at = low.find("mov rcx, [rip + pSwap]").expect("this -> rcx");
        let rdx_at = low.find("mov rdx,").expect("arg1 -> rdx");
        let r8_at = low.find("mov r8,").expect("arg2 -> r8");
        assert!(
            this_at > rdx_at && this_at > r8_at,
            "`this` must marshal into rcx AFTER the method's register args:\n{low}"
        );
    }

    #[test]
    fn equate_pass_skips_asciistring_shader_body() {
        let Some(kb) = kb() else { return };
        // A defined equate activates the preprocessing pass; an HLSL `x = y;` line inside
        // a raw .ASCIISTRING block must pass through verbatim, NOT be misparsed as a
        // `NAME = expr` equate (its comma once crashed the tokenizer). Real-code regression.
        let src = "N equ 5\n.asciistring\nfloat4 c = clamp(p, 0, 1);\n.endasciistring\n.code\n.globl m\nm:\n  mov eax, N\n  ret\n";
        let low = lower(src, &kb).expect("equate + .asciistring must lower, not crash");
        assert!(low.contains("mov eax, 5"), "the equate still folds: {low}");
    }

    #[test]
    fn equate_substitution_preserves_multibyte_string() {
        let Some(kb) = kb() else { return };
        // With an equate defined, every line runs through substitute_equates, which must
        // copy a quoted span by byte-range, not byte-by-byte: an em-dash (U+2014) in a
        // WCHAR string stays one UTF-16 unit (.word 8212), not split into its UTF-8 bytes.
        let src = "N equ 5\n.data\nt WCHAR \"\u{2014}\", 0\n.code\n.globl m\nm:\n  mov eax, N\n  ret\n";
        let low = lower(src, &kb).expect("equate + multibyte string must lower");
        assert!(low.contains(".word 8212"), "em-dash stays one UTF-16 unit: {low}");
        assert!(!low.contains("226"), "not split into its UTF-8 bytes: {low}");
    }

    #[test]
    fn check_treats_equates_as_known_with_a_shadow_note() {
        let Some(kb) = kb() else { return };
        // A clean equate is a known name — no 'unknown constant' diagnostic.
        let d = check(".code\n.globl main\nMYVAL equ 5\nmain:\n  mov eax, MYVAL\n  ret\n", &kb);
        assert!(d.is_empty(), "a clean equate produces no diagnostics: {d:?}");
        // An equate whose name matches a Windows constant is a SHADOW note (not an
        // 'unknown constant' error), reported once at the definition.
        let d = check(".code\n.globl main\nOFF equ 0\nmain:\n  mov eax, OFF\n  ret\n", &kb);
        assert!(!d.iter().any(|x| x.message.contains("unknown constant")), "no error: {d:?}");
        assert!(d.iter().any(|x| x.message.contains("shadows")), "a shadow note: {d:?}");
    }

    #[test]
    fn string_interpolation_folds_equates_into_strings() {
        let Some(kb) = kb() else { return };
        // `{{NAME}}` inside a "double-quoted" string interpolates the equate's value;
        // the rest of the string is verbatim. WCHAR 'v','3' lower to .word 118, .word 51.
        let low = lower(
            "VER equ 3\n.data\ns WCHAR \"v{{VER}}\", 0\n.code\n.globl m\nm:\n  ret\n",
            &kb,
        )
        .unwrap();
        assert!(!low.contains("{{"), "the placeholder is consumed: {low}");
        // WCHAR "v3" lowers to `.word 118, 51` (v, 3) — the {{VER}} folded to 3.
        assert!(low.contains("118, 51"), "v{{VER}} -> v3 interpolated: {low}");
        // The interpolator folds a known name and leaves an unknown one untouched.
        let mut eqs = std::collections::HashMap::new();
        eqs.insert("VER".to_string(), 3i64);
        assert_eq!(interpolate_equates("\"v{{VER}}\"", &eqs), "\"v3\"");
        assert_eq!(interpolate_equates("\"{{NOPE}}\"", &eqs), "\"{{NOPE}}\"");
    }

    #[test]
    fn check_severity_separates_shadow_warnings_from_real_errors() {
        let Some(kb) = kb() else { return };
        // The equate-shadow note is advisory — it must carry Severity::Warn.
        let d = check(".code\n.globl main\nOFF equ 0\nmain:\n  mov eax, OFF\n  ret\n", &kb);
        let shadow = d
            .iter()
            .find(|x| x.message.contains("shadows"))
            .expect("a shadow note");
        assert_eq!(shadow.severity, Severity::Warn, "shadow note is a warning: {shadow:?}");
        // A genuine unknown constant is a real error — Severity::Error. Use a
        // near-miss of a real Windows constant (OPEN_EXISTING) so the "did you
        // mean" suggestion fires and the diagnostic is produced.
        let d = check(".code\n.globl main\nmain:\n  mov eax, OPEN_EXISTNG\n  ret\n", &kb);
        let unknown = d
            .iter()
            .find(|x| x.message.contains("unknown constant 'OPEN_EXISTNG'"))
            .expect("an unknown-constant error");
        assert_eq!(unknown.severity, Severity::Error, "unknown constant is an error: {unknown:?}");
    }

    #[test]
    fn check_locates_encode_error_at_its_source_line() {
        let Some(kb) = kb() else { return };
        // Bogus mnemonic on line 4 (1-based).
        let src = ".globl main\nmain:\n  mov eax, 1\n  xyzzy rax, rbx\n  ret\n";
        let diags = check(src, &kb);
        assert!(diags.iter().any(|d| d.line == 4), "want a diag on line 4: {diags:?}");
        assert!(!diags.iter().any(|d| d.line == 0), "no whole-file fallback expected: {diags:?}");
    }

    #[test]
    fn check_maps_encode_error_back_through_invoke_expansion() {
        let Some(kb) = kb() else { return };
        // The `invoke` on line 3 expands to the whole Win64 call sequence (~13
        // lowered lines); the bad mnemonic on line 4 must still map back to 4.
        let src = ".globl main\nmain:\n  invoke ExitProcess, 7\n  xyzzy rax\n  ret\n";
        let diags = check(src, &kb);
        assert!(diags.iter().any(|d| d.line == 4), "want a diag on line 4: {diags:?}");
        assert!(!diags.iter().any(|d| d.line == 0), "{diags:?}");
    }

    #[test]
    fn check_clean_source_has_no_whole_file_diag() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  invoke ExitProcess, 0\n  ret\n";
        let diags = check(src, &kb);
        assert!(diags.is_empty(), "clean source should not flag anything: {diags:?}");
    }

    #[test]
    fn condition_parsing_and_branches() {
        assert_eq!(split_condition("al <= 'z'"), Some(("al ", "<=", " 'z'")));
        assert_eq!(split_condition("rcx < 26"), Some(("rcx ", "<", " 26")));
        assert_eq!(split_condition("eax s>= 0"), Some(("eax ", "s>=", " 0")));
        assert_eq!(split_condition("eax s< 0"), Some(("eax ", "s<", " 0")));
        assert!(split_condition("al z").is_none());
        // The branch is taken when the condition is `want`; `s` picks signed.
        assert_eq!(cond_branch("<=", false), Some("ja")); // unsigned exit
        assert_eq!(cond_branch("s<", false), Some("jge")); // signed exit
        assert_eq!(cond_branch("s<", true), Some("jl")); // signed, condition holds
        assert!(cond_branch("~~", false).is_none());
    }

    #[test]
    fn while_loop_lowers_to_branches_and_labels() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  mov al, 'a'\n  .while al <= 'z'\n    inc al\n  .endw\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        for want in ["__while0_top:", "cmp al, 'z'", "ja __while0_end", "jmp __while0_top", "__while0_end:"] {
            assert!(low.contains(want), "expansion missing {want:?}:\n{low}");
        }
        assert!(rasm::assemble(&low).is_ok(), "the expansion assembles:\n{low}");
    }

    #[test]
    fn if_elseif_else_lowers_and_assembles() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  mov al, 5\n  .if al == 1\n    mov bl, 10\n  .elseif al == 5\n    mov bl, 20\n  .else\n    mov bl, 30\n  .endif\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        for want in [
            "cmp al, 1", "jne __if0_1", "__if0_1:", "cmp al, 5", "jne __if0_2", "__if0_2:",
            "jmp __if0_end", "__if0_end:",
        ] {
            assert!(low.contains(want), "if-expansion missing {want:?}:\n{low}");
        }
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn repeat_until_lowers_and_assembles() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  xor cl, cl\n  .repeat\n    inc cl\n  .until cl == 10\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        for want in ["__repeat0_top:", "__repeat0_test:", "cmp cl, 10", "jne __repeat0_top", "__repeat0_end:"] {
            assert!(low.contains(want), "repeat-expansion missing {want:?}:\n{low}");
        }
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn break_targets_the_inner_loop_through_an_if() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  mov al, 'a'\n  .while al <= 'z'\n    .if al == 'm'\n      .break\n    .endif\n    inc al\n  .endw\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        // `.break` inside the `.if` inside the `.while` jumps to the while's end.
        assert!(low.contains("jmp __while0_end"), "break -> loop end:\n{low}");
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn for_loop_lowers_and_assembles() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  .for cl = 0 to 9\n    nop\n  .endfor\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        for want in [
            "mov cl, 0", "__for0_top:", "cmp cl, 9", "ja __for0_end", "__for0_cont:", "inc cl",
            "jmp __for0_top", "__for0_end:",
        ] {
            assert!(low.contains(want), "for-expansion missing {want:?}:\n{low}");
        }
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn forever_with_break_if_lowers_and_assembles() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  xor al, al\n  .forever\n    inc al\n    .break if al == 5\n  .endfor\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        for want in ["__forever0_top:", "cmp al, 5", "je __forever0_end", "jmp __forever0_top", "__forever0_end:"] {
            assert!(low.contains(want), "forever/break-if missing {want:?}:\n{low}");
        }
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn continue_if_targets_the_loop_test() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  mov al, 'a'\n  .while al <= 'z'\n    inc al\n    .continue if al == 'm'\n    nop\n  .endw\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        assert!(low.contains("je __while0_top"), "continue if -> loop test:\n{low}");
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn signed_while_uses_a_signed_branch() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  mov eax, 3\n  .while eax s> 0\n    nop\n  .endw\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        assert!(low.contains("jle __while0_end"), "signed `s>` exits with jle:\n{low}");
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn macro_header_and_names() {
        assert_eq!(
            parse_macro_def("PUSH2 MACRO a, b").unwrap(),
            ("PUSH2".to_string(), vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(parse_macro_def("NOARGS MACRO").unwrap(), ("NOARGS".to_string(), vec![]));
        assert!(parse_macro_def("mov rax, rbx").is_none());
        assert!(is_endm("ENDM") && is_endm("endm") && !is_endm("ret"));
        assert_eq!(macro_names("FOO MACRO x\n  nop\nENDM\n  FOO 1\n"), vec!["FOO".to_string()]);
    }

    #[test]
    fn macro_expands_args_and_definition_emits_no_code() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nPUSH2 MACRO a, b\n  push a\n  push b\nENDM\nmain:\n  PUSH2 rcx, rdx\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        assert!(low.contains("push rcx") && low.contains("push rdx"), "args substituted:\n{low}");
        assert!(!low.contains("MACRO"), "definition must emit nothing:\n{low}");
        assert!(!low.contains("PUSH2"), "invocation must be gone:\n{low}");
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn macro_can_use_high_level_constructs_twice() {
        let Some(kb) = kb() else { return };
        // A macro that loops; invoked twice → two independent loops (fresh ids).
        let src = ".globl main\nCOUNTDOWN MACRO n\n  mov al, n\n  .while al > 0\n    sub al, 1\n  .endw\nENDM\nmain:\n  COUNTDOWN 3\n  COUNTDOWN 5\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        assert!(low.contains("__while0_top:") && low.contains("__while1_top:"), "two loops:\n{low}");
        assert!(low.contains("mov al, 3") && low.contains("mov al, 5"), "args substituted:\n{low}");
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn macro_local_labels_unique_per_expansion() {
        let Some(kb) = kb() else { return };
        // A macro with a `LOCAL` label, invoked twice — no duplicate-symbol error.
        let src = ".globl main\nSKIP MACRO\n  LOCAL done\n  jmp done\ndone:\nENDM\nmain:\n  SKIP\n  SKIP\n  ret\n";
        let low = lower(src, &kb).expect("lower");
        assert!(low.contains("done__m0") && low.contains("done__m1"), "unique locals:\n{low}");
        assert!(rasm::assemble(&low).is_ok(), "no duplicate-label error:\n{low}");
    }

    #[test]
    fn macro_arg_count_mismatch_errors() {
        let Some(kb) = kb() else { return };
        let err = lower("M MACRO a, b\n  nop\nENDM\nmain:\n  M 1\n", &kb).unwrap_err();
        assert!(format!("{err:#}").contains("expects 2"), "{err:#}");
    }

    #[test]
    fn data_line_detection() {
        assert_eq!(parse_data_line("msg BYTE \"Hi\", 0"), Some((Some("msg"), "BYTE", "\"Hi\", 0")));
        assert_eq!(parse_data_line("count DWORD 0"), Some((Some("count"), "DWORD", "0")));
        assert_eq!(parse_data_line("WORD 1, 2"), Some((None, "WORD", "1, 2")));
        // A size-prefixed memory operand must NOT be read as data.
        assert_eq!(parse_data_line("mov BYTE PTR [rax], 1"), None);
        assert_eq!(parse_data_line("mov rax, rbx"), None);
    }

    #[test]
    fn data_lowers_and_assembles() {
        let Some(kb) = kb() else { return };
        let src = ".globl main\nmain:\n  lea rax, [rip + msg]\n  ret\nmsg BYTE \"Hi\", 0\ncount DWORD 7\ntable QWORD 1, 2\n";
        let low = lower(src, &kb).expect("lower");
        assert!(low.contains("msg:") && low.contains(".ascii \"Hi\"") && low.contains(".byte 0"), "{low}");
        assert!(low.contains("count:") && low.contains(".long 7"), "{low}");
        assert!(low.contains("table:") && low.contains(".quad 1") && low.contains(".quad 2"), "{low}");
        assert!(rasm::assemble(&low).is_ok(), "assembles:\n{low}");
    }

    #[test]
    fn wide_string_encodes_utf16() {
        let Some(kb) = kb() else { return };
        let low = lower("wmsg WCHAR \"Hi\", 0\n", &kb).expect("lower");
        assert!(low.contains(".word 72, 105"), "utf-16 code units:\n{low}");
        let m = rasm::assemble(&low).expect("assemble");
        // H(48 00) i(69 00) then the 00 00 terminator.
        assert_eq!(&m.code[..6], &[0x48, 0x00, 0x69, 0x00, 0x00, 0x00], "{:02x?}", m.code);
    }

    #[test]
    fn data_dup_and_uninitialized() {
        let Some(kb) = kb() else { return };
        let low = lower("buf BYTE 8 dup(0)\npad WORD ?\n", &kb).expect("lower");
        assert!(low.contains(".zero 8"), "dup(0) -> zero: {low}");
        assert!(low.contains(".zero 2"), "? -> zero of the field width: {low}");
        assert!(rasm::assemble(&low).is_ok(), "{low}");
    }

    #[test]
    fn data_size_validation_flags_overflow() {
        let Some(kb) = kb() else { return };
        let diags = check("x BYTE 256\ny WORD 70000\nz DWORD 0xFF\n", &kb);
        assert!(diags.iter().any(|d| d.line == 1 && d.message.contains("BYTE")), "{diags:?}");
        assert!(diags.iter().any(|d| d.line == 2 && d.message.contains("WORD")), "{diags:?}");
        assert!(!diags.iter().any(|d| d.line == 3), "0xFF fits a DWORD: {diags:?}");
    }

    #[test]
    fn mismatched_block_closers_error() {
        let Some(kb) = kb() else { return };
        assert!(lower("main:\n  .if al == 1\n  .endw\n", &kb).is_err(), ".endw must not close .if");
        assert!(lower("main:\n  .endif\n", &kb).is_err(), ".endif without .if");
        assert!(lower("main:\n  .endfor\n", &kb).is_err(), ".endfor without .for/.forever");
        assert!(lower("main:\n  .break\n", &kb).is_err(), ".break outside a loop");
        assert!(lower("main:\n  .until al == 1\n", &kb).is_err(), ".until without .repeat");
    }

    // ── compile-time equates + conditional assembly ──────────────────────────

    #[test]
    fn equate_arithmetic_folds_into_operand() {
        let Some(kb) = kb() else { return };
        let low = lower("main:\n  N equ 8*4\n  mov eax, N\n", &kb).unwrap();
        assert!(low.contains("mov eax, 32"), "N equ 8*4 should fold to 32:\n{low}");
        // the definition line emits nothing
        assert!(!low.contains("equ"), "the equate definition must not survive:\n{low}");
    }

    #[test]
    fn equate_hex_and_bitwise_or() {
        let Some(kb) = kb() else { return };
        let low = lower("main:\n  F equ 0x10 | 1\n  mov eax, F\n", &kb).unwrap();
        assert!(low.contains("mov eax, 17"), "0x10 | 1 == 17:\n{low}");
    }

    #[test]
    fn equate_table_folds_and_records_lines() {
        let Some(kb) = kb() else { return };
        let table = equate_table("W equ 4\nH equ 5\nA equ W * H\n", &kb);
        let names: Vec<&str> = table.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["W", "H", "A"]);
        let a = table.iter().find(|e| e.name == "A").expect("A is in the table");
        assert_eq!(a.value, 20, "A equ W * H == 4 * 5 == 20: {table:?}");
        assert!(a.expr.contains("W * H"), "A keeps its source expr: {:?}", a.expr);
        assert_eq!(a.line, 3, "A is defined on the 3rd line");
        // The earlier equates fold to their literals.
        assert_eq!(table.iter().find(|e| e.name == "W").unwrap().value, 4);
        assert_eq!(table.iter().find(|e| e.name == "H").unwrap().value, 5);
    }

    #[test]
    fn eval_expr_precedence_and_operators() {
        let Some(kb) = kb() else { return };
        let e = HashMap::new();
        assert_eq!(eval_expr("2 + 3 * 4", &e, &kb).unwrap(), 14);
        assert_eq!(eval_expr("(2 + 3) * 4", &e, &kb).unwrap(), 20);
        assert_eq!(eval_expr("1 << 4", &e, &kb).unwrap(), 16);
        assert_eq!(eval_expr("0xFF & 0x0F", &e, &kb).unwrap(), 0x0F);
        assert_eq!(eval_expr("~0 & 0xFF", &e, &kb).unwrap(), 0xFF);
        assert_eq!(eval_expr("-5 + 8", &e, &kb).unwrap(), 3);
        assert_eq!(eval_expr("10 % 3", &e, &kb).unwrap(), 1);
        assert_eq!(eval_expr("1 | 2 ^ 2", &e, &kb).unwrap(), 1); // ^ binds tighter than |
        assert!(eval_expr("3 +", &e, &kb).is_err());
        assert!(eval_expr("totally_unknown_name", &e, &kb).is_err());
    }

    #[test]
    fn equate_resolves_winkb_constants() {
        let Some(kb) = kb() else { return };
        // Pick a pair that exists and fold them with `|`; cross-check kb.resolve.
        let ok = kb.resolve("MB_OK").unwrap();
        let err = kb.resolve("MB_ICONERROR").unwrap();
        let (Some(a), Some(b)) = (ok.first(), err.first()) else { return };
        let want = a.i64v | b.i64v;
        let src = format!("main:\n  S equ MB_OK | MB_ICONERROR\n  mov eax, S\n");
        let low = lower(&src, &kb).unwrap();
        assert!(low.contains(&format!("mov eax, {want}")), "want folded {want}:\n{low}");
    }

    #[test]
    fn dup_count_folds_via_equate_trap5_retired() {
        let Some(kb) = kb() else { return };
        // STRIDE used as a `dup` count: must fold to a literal before lower_data
        // (this is exactly what retires trap #5 — a dup count no longer has to be
        // a bare literal in the source).
        let low = lower(".data\nSTRIDE equ 32\nbuf BYTE STRIDE dup(0)\n", &kb).unwrap();
        assert!(low.contains(".zero 32"), "BYTE STRIDE dup(0) must fold to .zero 32:\n{low}");
    }

    #[test]
    fn equate_is_not_substituted_inside_a_string() {
        let Some(kb) = kb() else { return };
        // `N` is an equate, but the WCHAR string literally contains the word N.
        let low = lower("N equ 5\nmsg WCHAR \"N items\", 0\n", &kb).unwrap();
        // The 'N' (0x4E) and 'i','t'... encode as their UTF-16 units; the equate's
        // value 5 must NOT appear where the string's 'N' is.
        assert!(low.contains("78"), "the literal 'N' (78) must survive in the string:\n{low}");
        assert!(!low.contains(".word 5,") && !low.contains(", 5,"), "no equate sub in string:\n{low}");
    }

    #[test]
    fn conditional_if_includes_and_excludes() {
        let Some(kb) = kb() else { return };
        let on = lower("main:\n  DBG equ 1\n  IF DBG\n  mov eax, 111\n  ELSE\n  mov eax, 222\n  ENDIF\n", &kb).unwrap();
        assert!(on.contains("mov eax, 111") && !on.contains("mov eax, 222"), "DBG=1 keeps A:\n{on}");
        let off = lower("main:\n  DBG equ 0\n  IF DBG\n  mov eax, 111\n  ELSE\n  mov eax, 222\n  ENDIF\n", &kb).unwrap();
        assert!(off.contains("mov eax, 222") && !off.contains("mov eax, 111"), "DBG=0 keeps B:\n{off}");
    }

    #[test]
    fn conditional_ifdef_ifndef_and_nesting() {
        let Some(kb) = kb() else { return };
        // FEATURE defined -> outer taken; nested IF on its value selects inner A.
        let src = "FEATURE equ 1\nmain:\n  IFDEF FEATURE\n    IF FEATURE\n    mov eax, 1\n    ELSE\n    mov eax, 2\n    ENDIF\n  ELSE\n  mov eax, 3\n  ENDIF\n";
        let low = lower(src, &kb).unwrap();
        assert!(low.contains("mov eax, 1"), "nested taken branch:\n{low}");
        assert!(!low.contains("mov eax, 2") && !low.contains("mov eax, 3"), "others dropped:\n{low}");
        // IFNDEF of an undefined name is taken.
        let low2 = lower("main:\n  IFNDEF NOPE\n  mov eax, 9\n  ENDIF\n", &kb).unwrap();
        assert!(low2.contains("mov eax, 9"), "IFNDEF undefined is taken:\n{low2}");
    }

    #[test]
    fn runtime_dot_if_still_lowers_after_equates() {
        let Some(kb) = kb() else { return };
        // The compile-time IF and the runtime `.if` coexist; `.if` must still
        // lower to a compare + branch.
        let src = "LIMIT equ 5\nmain:\n  mov eax, 0\n  .if eax < LIMIT\n    inc eax\n  .endif\n  ret\n";
        let low = lower(src, &kb).unwrap();
        assert!(low.contains("cmp eax, 5"), "LIMIT folds into the runtime cmp:\n{low}");
        assert!(low.contains("__if0_"), "runtime .if still emits its branch labels:\n{low}");
    }

    #[test]
    fn equate_in_skipped_branch_is_not_recorded() {
        let Some(kb) = kb() else { return };
        // X is defined only inside a dropped branch, so it must NOT fold: the use
        // survives verbatim as a bare symbol (left for rasm to treat as a label).
        let src = "main:\n  IF 0\n  X equ 7\n  ENDIF\n  mov eax, X\n";
        let low = lower(src, &kb).unwrap();
        assert!(low.contains("mov eax, X"), "X from a skipped branch must not fold:\n{low}");
        assert!(!low.contains("mov eax, 7"), "the dropped equate must not leak:\n{low}");
    }

    #[test]
    fn struct_field_equals_is_not_an_equate() {
        let Some(kb) = kb() else { return };
        // Inside a `… struct … ends` block, `Format = 87` is a field, not an
        // equate; the preprocessor must NOT swallow it as a definition.
        let src = "scd struct DXGI_SWAP_CHAIN_DESC\n  BufferDesc.Format = 87\n  BufferCount = 2\nends\n";
        let low = lower(src, &kb).unwrap();
        assert!(low.contains("scd:"), "the struct instance must be laid out:\n{low}");
        // 87 lands as data, not vanished into an equate table.
        assert!(low.contains("87"), "the field value must reach the layout:\n{low}");
    }

    #[test]
    fn equate_line_map_points_at_real_source_line() {
        let Some(kb) = kb() else { return };
        // Equates + a conditional precede a bogus mnemonic on line 7; the diag
        // must report line 7, mirroring check_locates_encode_error_at_its_source_line.
        let src = ".globl main\nN equ 8\nmain:\n  IF N\n  mov eax, N\n  ENDIF\n  xyzzy rax, rbx\n  ret\n";
        let diags = check(src, &kb);
        assert!(diags.iter().any(|d| d.line == 7), "want a diag on source line 7: {diags:?}");
        assert!(!diags.iter().any(|d| d.line == 0), "no whole-file fallback: {diags:?}");
    }

    #[test]
    fn shift_out_of_range_is_a_clean_error_not_a_panic() {
        let Some(kb) = kb() else { return };
        let e = HashMap::new();
        for expr in ["1 << 64", "1 << 100", "1 << -1", "1 >> 64", "1 >> -1"] {
            let r = eval_expr(expr, &e, &kb);
            assert!(r.is_err(), "`{expr}` must be a clean error, got {r:?}");
            assert!(
                format!("{:#}", r.unwrap_err()).contains("shift count"),
                "`{expr}` should mention the shift count"
            );
        }
        // In-range shifts still work.
        assert_eq!(eval_expr("1 << 63", &e, &kb).unwrap(), i64::MIN);
        // Logical right shift: a negative left operand does NOT sign-extend.
        assert_eq!(eval_expr("(0 - 8) >> 1", &e, &kb).unwrap(), (((-8i64) as u64) >> 1) as i64);
        // Reachable through the public lower() entrypoint without panicking.
        assert!(lower("FLAGS equ 1 << 64\nmain:\n  mov eax, FLAGS\n", &kb).is_err());
    }

    #[test]
    fn division_overflow_is_a_clean_error_not_a_panic() {
        let Some(kb) = kb() else { return };
        let e = HashMap::new();
        let div = eval_expr("(0 - 9223372036854775807 - 1) / (0 - 1)", &e, &kb);
        assert!(div.is_err(), "i64::MIN / -1 must error, got {div:?}");
        assert!(format!("{:#}", div.unwrap_err()).contains("overflow"));
        let rem = eval_expr("(0 - 9223372036854775807 - 1) % (0 - 1)", &e, &kb);
        assert!(rem.is_err(), "i64::MIN % -1 must error, got {rem:?}");
        // Plain division still works.
        assert_eq!(eval_expr("100 / 7", &e, &kb).unwrap(), 14);
    }

    #[test]
    fn ifdef_resolves_only_the_first_token() {
        let Some(kb) = kb() else { return };
        // IFDEF with a trailing junk token on a DEFINED symbol must still take it.
        let on = lower("X equ 1\nmain:\n  IFDEF X extra junk\n  mov eax, 1\n  ENDIF\n", &kb).unwrap();
        assert!(on.contains("mov eax, 1"), "IFDEF X (defined) must include, trailing junk ignored:\n{on}");
        // IFNDEF with a trailing token on a DEFINED symbol must NOT take it.
        let off = lower("X equ 1\nmain:\n  IFNDEF X trailing\n  mov eax, 777\n  ENDIF\n", &kb).unwrap();
        assert!(!off.contains("mov eax, 777"), "IFNDEF of a defined X must drop the branch:\n{off}");
    }

    #[test]
    fn register_named_equate_is_rejected() {
        let Some(kb) = kb() else { return };
        let r = lower("eax equ 99\nmain:\n  mov eax, 1\n", &kb);
        assert!(r.is_err(), "a register-named equate must be rejected, not silently fold");
        assert!(format!("{:#}", r.unwrap_err()).contains("shadows a register"));
    }

    #[test]
    fn equate_name_inside_a_comment_is_not_rewritten() {
        let Some(kb) = kb() else { return };
        // The comment text mentions the equate name and contains a lone apostrophe;
        // neither the name nor the apostrophe-driven quote scan must touch the code.
        let low = lower("N equ 5\nmain:\n  mov eax, N  ; that's N items\n", &kb).unwrap();
        assert!(low.contains("mov eax, 5"), "the code N folds:\n{low}");
    }

    #[test]
    fn unclosed_if_points_at_the_opening_line() {
        let Some(kb) = kb() else { return };
        let src = "main:\n  mov eax, 1\n  IF 1\n  mov eax, 2\n";
        let diags = check(src, &kb);
        assert!(diags.iter().any(|d| d.line == 3), "unclosed IF should point at line 3: {diags:?}");
    }
}
