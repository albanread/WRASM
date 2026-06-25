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

/// Run the inliner over lowered asm. Returns the rewritten text plus an `origin`
/// map (`origin[i]` = the 0-based input line the output line `i` derives from), so
/// the caller can keep error line-mapping intact.
///
/// Phase 1: the identity buffer — parse then render, no passes. Behaviour-neutral
/// by the round-trip contract; later commits add inline_calls + the cleanup passes.
pub fn optimize(asm: &str, _threshold: usize) -> (String, Vec<usize>) {
    let lines = parse(asm);
    let origin: Vec<usize> = (0..lines.len()).collect();
    (render(&lines), origin)
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
