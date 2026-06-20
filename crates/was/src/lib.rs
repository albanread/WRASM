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
fn lower_mapped(src: &str, kb: &Kb) -> Result<(String, Vec<usize>)> {
    let labels = collect_labels(src);
    let mut out = String::new();
    let mut map: Vec<usize> = Vec::new();
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
}
