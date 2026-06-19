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

use anyhow::{bail, Result};
use std::collections::HashSet;
use winkb::Kb;

/// First four integer/pointer argument registers (Win64).
const ARG_REGS: [&str; 4] = ["rcx", "rdx", "r8", "r9"];

/// Lower `src` to rasm-ready Intel-syntax text.
pub fn lower(src: &str, kb: &Kb) -> Result<String> {
    let labels = collect_labels(src);
    let mut out = String::new();
    for raw in src.lines() {
        let body = strip_comment(raw);
        let t = body.trim();
        if t.is_empty() {
            out.push('\n');
            continue;
        }
        // `invoke Func, args…`
        if let Some(rest) = strip_keyword(t, "invoke") {
            out.push_str(&expand_invoke(rest, kb, &labels)?);
            continue;
        }
        // Directives pass through untouched.
        if t.starts_with('.') {
            out.push_str(body);
            out.push('\n');
            continue;
        }
        // Instruction (possibly with a leading `label:`): resolve operands.
        out.push_str(&rewrite_line(body, kb, &labels)?);
        out.push('\n');
    }
    Ok(out)
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
}
