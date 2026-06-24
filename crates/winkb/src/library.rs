//! Library symbol index — the public API of the WRASM standard library.
//!
//! The assembler's `module NAME … endmodule` regions give every label a
//! visibility: a name whose first letter is **uppercase is PUBLIC** (the module's
//! exported API); a lowercase/`_` name is private (mangled `Module$name`). A
//! module is independent of files — `module Canvas` is split across
//! `blit/canvas/fx/…`, pooling by name.
//!
//! [`scan_was`] sweeps one file's module regions for the public symbols and what
//! we can say about each — its module, location, kind, a `proc`'s in/out/uses
//! signature, and the doc comment above it. The `import_library` bin walks
//! `library/` + `gpu/` and writes them to the `library_symbols` table, which
//! [`crate::Kb::library_symbols`] reads back so the IDE can answer "where does
//! `Blit` come from?" for your own code, the way it already does for Windows.

/// One public symbol defined inside a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySymbol {
    pub name: String,
    pub module: String,
    pub file: String,
    /// 1-based line of the definition within `file`.
    pub line: usize,
    /// `proc` | `data` | `equate` | `label` | `macro`.
    pub kind: String,
    /// For a `proc`, a readable `in … · out … · uses … · frame`; else empty.
    pub signature: String,
    /// The doc comment immediately above the definition (cleaned), or empty.
    pub summary: String,
}

/// Scan one `.was`/`.inc` file's `module … endmodule` regions for PUBLIC symbols
/// (first letter uppercase). `file` is stored verbatim with each symbol (use a
/// repo-relative, forward-slash path). Lines outside any module are ignored — the
/// public/private rule only exists inside modules.
pub fn scan_was(file: &str, text: &str) -> Vec<LibrarySymbol> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut module: Option<String> = None; // the module owning the current line

    for (i, raw) in lines.iter().enumerate() {
        let t = strip_comment(raw).trim();
        let mut words = t.split_whitespace();
        match words.next() {
            Some("module") => {
                module = words.next().map(str::to_string);
                continue;
            }
            Some("endmodule") => {
                module = None;
                continue;
            }
            _ => {}
        }
        let Some(m) = module.as_deref() else { continue };
        let Some((name, kind, signature)) = def_on_line(t) else { continue };
        if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            continue; // lowercase first letter → private
        }
        out.push(LibrarySymbol {
            name: name.to_string(),
            module: m.to_string(),
            file: file.to_string(),
            line: i + 1,
            kind: kind.to_string(),
            signature,
            summary: doc_above(&lines, i),
        });
    }
    out
}

/// A symbol definition on `t`: `(name, kind, signature)`. Recognises `proc`,
/// `macro`, a `name:` label, `name equ/= value`, and a `name TYPE …` data slot.
fn def_on_line(t: &str) -> Option<(&str, &'static str, String)> {
    // proc NAME [frame] [uses R…] [in R…] [out R…]
    if let Some(rest) = strip_kw(t, "proc") {
        let name = rest.split_whitespace().next()?;
        let sig = proc_signature(rest[name.len()..].trim());
        return Some((name, "proc", sig));
    }
    if let Some(rest) = strip_kw(t, "macro") {
        return Some((rest.split_whitespace().next()?, "macro", String::new()));
    }
    // NAME:  (a label, optionally followed by an instruction)
    if let Some(name) = leading_label(t) {
        return Some((name, "label", String::new()));
    }
    let mut words = t.split_whitespace();
    let name = words.next().filter(|w| is_ident(w))?;
    let op = words.next()?;
    if op.eq_ignore_ascii_case("equ") || op == "=" {
        let value = t[name.len()..].trim_start();
        let value = strip_kw(value, "equ").unwrap_or_else(|| value.trim_start_matches('=').trim());
        return Some((name, "equate", value.trim().to_string()));
    }
    // NAME <TYPE> …  — but not a `dword ptr` size override (that's an operand).
    if is_data_type(op) && words.next() != Some("ptr") {
        return Some((name, "data", op.to_ascii_uppercase()));
    }
    None
}

/// Build a readable signature from a `proc` header's tail (after the name).
fn proc_signature(tail: &str) -> String {
    let (mut frame, mut bucket) = (false, 0u8);
    let (mut uses, mut ins, mut outs) = (Vec::new(), Vec::new(), Vec::new());
    for tok in tail.split_whitespace() {
        match tok {
            "frame" => frame = true,
            "uses" => bucket = 1,
            "in" => bucket = 2,
            "out" => bucket = 3,
            r => match bucket {
                1 => uses.push(r),
                2 => ins.push(r),
                3 => outs.push(r),
                _ => {}
            },
        }
    }
    let mut parts = Vec::new();
    if !ins.is_empty() {
        parts.push(format!("in {}", ins.join(" ")));
    }
    if !outs.is_empty() {
        parts.push(format!("out {}", outs.join(" ")));
    }
    if !uses.is_empty() {
        parts.push(format!("uses {}", uses.join(" ")));
    }
    if frame {
        parts.push("frame".to_string());
    }
    parts.join(" · ")
}

/// The cleaned doc comment immediately above line `i` (the contiguous run of `;`
/// lines, stopping at a blank/code line or a pure separator rule).
fn doc_above(lines: &[&str], i: usize) -> String {
    let mut block: Vec<String> = Vec::new();
    let mut j = i;
    while j > 0 {
        j -= 1;
        let l = lines[j].trim();
        let Some(c) = l.strip_prefix(';') else { break };
        let c = c.trim().trim_matches(|ch| ch == '-' || ch == '=' || ch == ' ');
        if c.is_empty() {
            break; // a separator / blank comment ends the doc block
        }
        block.push(c.to_string());
    }
    block.reverse();
    block.join(" ")
}

// ── small syntax helpers (kept self-contained so winkb stays a leaf crate) ──

/// `kw` followed by whitespace at the start of `t` → the remainder, else None.
fn strip_kw<'a>(t: &'a str, kw: &str) -> Option<&'a str> {
    let rest = t.strip_prefix(kw)?;
    rest.starts_with(|c: char| c.is_whitespace()).then(|| rest.trim_start())
}

/// `NAME:` at the start of `t` → NAME (a label definition).
fn leading_label(t: &str) -> Option<&str> {
    let (head, _) = t.split_once(':')?;
    let head = head.trim();
    (is_ident(head) && !head.is_empty()).then_some(head)
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '.')
        && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$')
}

fn is_data_type(s: &str) -> bool {
    matches!(
        s.trim_start_matches('.').to_ascii_lowercase().as_str(),
        "byte" | "sbyte" | "word" | "sword" | "dword" | "sdword" | "qword" | "sqword" | "tbyte"
            | "wchar" | "db" | "dw" | "dd" | "dq" | "dt" | "real4" | "real8" | "real10" | "single"
            | "double"
    )
}

/// Drop a trailing `;` line comment (respecting nothing fancy — library source
/// has no `;` inside strings on definition lines).
fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(i) => &line[..i],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_public_module_symbols_with_signatures_and_docs() {
        let src = "\
module Canvas
.DATA
dstBase   QWORD ?            ; private — lowercase, skipped
Palette   DWORD 0           ; public data
.CODE
; ---- Blit: opaque rectangular copy ----
proc Blit frame in rcx rdx r8 r9
    ret
endproc
; a private helper
proc blitEx
    ret
endproc
endmodule
mov dword ptr [rip + x], 1   ; outside any module — ignored
";
        let syms = scan_was("library/blit.was", src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Palette", "Blit"], "only PUBLIC, in-module symbols");

        let blit = syms.iter().find(|s| s.name == "Blit").unwrap();
        assert_eq!(blit.module, "Canvas");
        assert_eq!(blit.file, "library/blit.was");
        assert_eq!(blit.kind, "proc");
        assert_eq!(blit.signature, "in rcx rdx r8 r9 · frame");
        assert_eq!(blit.summary, "Blit: opaque rectangular copy");

        let pal = syms.iter().find(|s| s.name == "Palette").unwrap();
        assert_eq!(pal.kind, "data");
        assert_eq!(pal.signature, "DWORD");
    }

    #[test]
    fn equates_and_a_dword_ptr_override() {
        let src = "module M\nCANVAS_W equ 320\n  mov dword ptr [rip + p], 1\nendmodule\n";
        let syms = scan_was("f.was", src);
        // CANVAS_W is a public equate; the `mov dword ptr` line defines nothing.
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].kind, "equate");
        assert_eq!(syms[0].signature, "320");
    }
}
