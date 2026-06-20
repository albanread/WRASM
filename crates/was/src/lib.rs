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
use std::collections::HashSet;
use winkb::Kb;

/// First four integer/pointer argument registers (Win64).
const ARG_REGS: [&str; 4] = ["rcx", "rdx", "r8", "r9"];

/// One diagnostic: a 1-based line/column and a message.
#[derive(Debug, Clone)]
pub struct Diag {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

/// Check `src` and return diagnostics — semantic (invoke arg count, unknown
/// constants with a "did you mean", bad struct fields) plus a whole-file
/// syntax/encode pass through rasm. Empty result = clean.
pub fn check(src: &str, kb: &Kb) -> Vec<Diag> {
    let labels = collect_labels(src);
    let mut diags = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let body = strip_comment(raw);
        let t = body.trim();
        if t.is_empty() {
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
                    });
                }
            }
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
        Diag { line, col, message }
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
                        if !field.is_empty() && !layout.fields.iter().any(|f| f.name == field) {
                            let near = layout
                                .fields
                                .iter()
                                .min_by_key(|f| lev(field, &f.name))
                                .map(|f| format!(" — did you mean '{}.{}'?", lhs, f.name))
                                .unwrap_or_default();
                            diags.push(Diag {
                                line,
                                col,
                                message: format!("{lhs} has no field '{field}'{near}"),
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

/// Lower `src` to rasm-ready Intel-syntax text.
pub fn lower(src: &str, kb: &Kb) -> Result<String> {
    Ok(lower_mapped(src, kb)?.0)
}

/// Lower `src`, also returning a map from each *lowered* line (0-based) back to
/// the 1-based source line it came from. One source line can expand to many
/// lowered lines (an `invoke` becomes the whole Win64 call sequence), so this is
/// what lets [`check`] point a downstream `rasm::assemble` error — whose line
/// numbers are lowered-line numbers — at the real source line. Lowering errors
/// are tagged ``line N: …`` with the *source* line directly.
pub fn lower_mapped(src: &str, kb: &Kb) -> Result<(String, Vec<usize>)> {
    let labels = collect_labels(src);
    let mut out = String::new();
    let mut map: Vec<usize> = Vec::new();
    // High-level block state: a counter for unique labels, and a stack so each
    // `.endX` matches its opener and `.break`/`.continue` find the inner loop.
    let mut block_ctr = 0usize;
    let mut block_stack: Vec<Block> = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let src_line = i + 1;
        let start = out.len();
        let body = strip_comment(raw);
        let t = body.trim();
        if t.is_empty() {
            out.push('\n');
        } else if let Some(rest) = strip_keyword(t, "invoke") {
            // `invoke Func, args…`
            let expanded = expand_invoke(rest, kb, &labels)
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
        } else if t.starts_with('.') {
            // Directives pass through untouched.
            out.push_str(body);
            out.push('\n');
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
        map.resize(map.len() + added, src_line);
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
            Block::If { .. } => return None,
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
            if let Some(layout) = kb.layout(lhs)? {
                if let Some(f) = layout.fields.iter().find(|f| f.name == field) {
                    return Ok(f.offset.to_string());
                }
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
fn expand_invoke(rest: &str, kb: &Kb, labels: &HashSet<String>) -> Result<String> {
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
                emit_call(func, &args, kb, labels)?
            ));
        }
    }
    emit_call(func, &args, kb, labels)
}

fn emit_call(func: &str, args: &[String], kb: &Kb, labels: &HashSet<String>) -> Result<String> {
    let n = args.len();
    let stack_args = n.saturating_sub(4);
    let stack_bytes = stack_args * 8;
    let frame = 32 + ((stack_bytes + 15) & !15); // shadow space + aligned stack args

    let mut o = String::new();
    o.push_str(&format!("  ; invoke {func} ({n} args)\n"));
    o.push_str("  push rbx\n  mov rbx, rsp\n  and rsp, -16\n");
    o.push_str(&format!("  sub rsp, {frame}\n"));

    // Stack args (index >= 4), high to low is irrelevant; place at [rsp+32+...].
    for (idx, arg) in args.iter().enumerate().skip(4) {
        let off = 32 + (idx - 4) * 8;
        o.push_str(&load_arg("rax", arg, kb, labels)?);
        o.push_str(&format!("  mov [rsp + {off}], rax\n"));
    }
    // Register args (0..=3).
    for (idx, arg) in args.iter().enumerate().take(4) {
        o.push_str(&load_arg(ARG_REGS[idx], arg, kb, labels)?);
    }
    o.push_str(&format!("  call {func}\n"));
    o.push_str("  mov rsp, rbx\n  pop rbx\n");
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

// ── small helpers ────────────────────────────────────────────────────────────

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn strip_comment(line: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in line.char_indices() {
        match c {
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
    fn split_commas_respects_brackets() {
        let p = split_top_commas("Func, [rsp + 8], 0x10, msg");
        assert_eq!(p, vec!["Func", " [rsp + 8]", " 0x10", " msg"]);
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
    fn mismatched_block_closers_error() {
        let Some(kb) = kb() else { return };
        assert!(lower("main:\n  .if al == 1\n  .endw\n", &kb).is_err(), ".endw must not close .if");
        assert!(lower("main:\n  .endif\n", &kb).is_err(), ".endif without .if");
        assert!(lower("main:\n  .endfor\n", &kb).is_err(), ".endfor without .for/.forever");
        assert!(lower("main:\n  .break\n", &kb).is_err(), ".break outside a loop");
        assert!(lower("main:\n  .until al == 1\n", &kb).is_err(), ".until without .repeat");
    }
}
