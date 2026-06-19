//! syntax.rs — a winkb-aware asm syntax lexer for the editor.
//!
//! Pure and per-line: `lex_line(line)` returns typed [`Token`]s with byte spans,
//! so the renderer colours sub-ranges and the IDE can reason about what's there.
//! It shares the assembler's own classifiers — [`rasm::is_register`] and
//! [`rasm::looks_like_number`] — so a highlighted register/number is exactly one
//! `rasm` will accept; there is no second table to drift.
//!
//! Constants are only recognized *structurally* here: an `UPPER_SNAKE` operand
//! becomes [`TokKind::Constant`]. Whether it actually resolves in winkb — the
//! colour that makes a typo'd constant stand out — is a separate semantic pass
//! on the language thread (it is what `was::check` already reports), kept off the
//! per-keystroke path so highlighting stays instant.

/// The lexical class of a token, for colouring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    /// A `;` or `#` end-of-line comment.
    Comment,
    /// A `name:` label at the head of a line.
    Label,
    /// A `.text` / `.globl` / … assembler directive.
    Directive,
    /// The instruction opcode word.
    Mnemonic,
    /// `invoke`, size words (`byte`/`word`/`dword`/`qword`/…), `ptr`, `sizeof`.
    Keyword,
    /// A register: GPR (8/16/32/64), vector (xmm/ymm/zmm), or `rip`.
    Register,
    /// An integer literal (`42`, `0x1F`).
    Number,
    /// A `"…"` / `'…'` string literal.
    String,
    /// An `UPPER_SNAKE` operand — a Windows constant/enum member by shape.
    Constant,
    /// Any other word: a symbol, a label reference, a `Struct.field`.
    Ident,
    /// Punctuation: `, [ ] + - * : ( )` etc.
    Punct,
}

/// A classified token: `line[start..end]` is its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
}

/// Lex one line into tokens (whitespace produces no token).
pub fn lex_line(line: &str) -> Vec<Token> {
    let raws = raw_tokens(line);
    classify(line, &raws)
}

/// Lex a whole buffer, one token row per line — the shape an editor keeps
/// alongside its lines (cf. WF66's `fedit`).
pub fn lex(text: &str) -> Vec<Vec<Token>> {
    text.lines().map(lex_line).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Raw {
    Word,
    Punct,
    Str,
    Comment,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c == '$' || c == '@'
}

/// First pass: split into raw lexemes (no semantics yet), tracking `[]` depth so
/// a `;`/`#` inside a memory operand isn't mistaken for a comment.
fn raw_tokens(line: &str) -> Vec<(Raw, usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut depth = 0i32;
    while i < line.len() {
        let c = line[i..].chars().next().unwrap();
        let cl = c.len_utf8();
        if c.is_whitespace() {
            i += cl;
            continue;
        }
        if (c == ';' || c == '#') && depth == 0 {
            out.push((Raw::Comment, i, line.len()));
            break;
        }
        if c == '"' || c == '\'' {
            let start = i;
            i += cl;
            while i < line.len() {
                let d = line[i..].chars().next().unwrap();
                i += d.len_utf8();
                if d == '\\' {
                    if i < line.len() {
                        i += line[i..].chars().next().unwrap().len_utf8();
                    }
                    continue;
                }
                if d == c {
                    break;
                }
            }
            out.push((Raw::Str, start, i));
            continue;
        }
        if is_word_char(c) {
            let start = i;
            while i < line.len() {
                let d = line[i..].chars().next().unwrap();
                if is_word_char(d) {
                    i += d.len_utf8();
                } else {
                    break;
                }
            }
            out.push((Raw::Word, start, i));
            continue;
        }
        if c == '[' {
            depth += 1;
        } else if c == ']' {
            depth -= 1;
        }
        out.push((Raw::Punct, i, i + cl));
        i += cl;
    }
    out
}

/// Second pass: assign a [`TokKind`] using line position (head word = mnemonic
/// or directive; a leading `name:` is a label; everything after the head is an
/// operand).
fn classify(line: &str, raws: &[(Raw, usize, usize)]) -> Vec<Token> {
    // A leading label is `Word` then `Punct(":")`.
    let has_label = raws.len() >= 2
        && raws[0].0 == Raw::Word
        && raws[1].0 == Raw::Punct
        && &line[raws[1].1..raws[1].2] == ":";
    let head = if has_label { 2 } else { 0 };

    let mut out = Vec::with_capacity(raws.len());
    for (k, &(raw, s, e)) in raws.iter().enumerate() {
        let text = &line[s..e];
        let kind = match raw {
            Raw::Comment => TokKind::Comment,
            Raw::Str => TokKind::String,
            Raw::Punct => TokKind::Punct,
            Raw::Word if has_label && k == 0 => TokKind::Label,
            Raw::Word if k == head => {
                if text.starts_with('.') {
                    TokKind::Directive
                } else if text.eq_ignore_ascii_case("invoke") {
                    TokKind::Keyword
                } else {
                    TokKind::Mnemonic
                }
            }
            Raw::Word => operand_kind(text),
        };
        out.push(Token { kind, start: s, end: e });
    }
    out
}

/// Classify a word appearing in operand position.
fn operand_kind(w: &str) -> TokKind {
    if rasm::is_register(w) {
        TokKind::Register
    } else if rasm::looks_like_number(w) {
        TokKind::Number
    } else if is_size_keyword(w) || w.eq_ignore_ascii_case("ptr") || w.eq_ignore_ascii_case("sizeof")
    {
        TokKind::Keyword
    } else if is_constant_like(w) {
        TokKind::Constant
    } else {
        TokKind::Ident
    }
}

fn is_size_keyword(w: &str) -> bool {
    matches!(
        w.to_ascii_lowercase().as_str(),
        "byte" | "word" | "dword" | "qword" | "tbyte" | "oword" | "xmmword" | "ymmword" | "zmmword"
    )
}

/// `UPPER_SNAKE` shape: only A–Z / 0–9 / `_`, with at least one letter, length
/// > 1. Matches the convention Windows constants and enum members follow.
fn is_constant_like(w: &str) -> bool {
    w.len() > 1
        && w.chars().any(|c| c.is_ascii_uppercase())
        && w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect (kind, text) pairs for easy assertions.
    fn toks(line: &str) -> Vec<(TokKind, &str)> {
        lex_line(line).into_iter().map(|t| (t.kind, &line[t.start..t.end])).collect()
    }

    #[test]
    fn mnemonic_register_number() {
        assert_eq!(
            toks("mov rax, 42"),
            vec![
                (TokKind::Mnemonic, "mov"),
                (TokKind::Register, "rax"),
                (TokKind::Punct, ","),
                (TokKind::Number, "42"),
            ]
        );
    }

    #[test]
    fn comment_to_end_of_line() {
        let t = toks("ret ; done here");
        assert_eq!(t[0], (TokKind::Mnemonic, "ret"));
        assert_eq!(t[1], (TokKind::Comment, "; done here"));
    }

    #[test]
    fn comment_hash_not_inside_brackets() {
        // '#' at top level is a comment; inside [] it would not be — but no real
        // operand uses '#', so just check the top-level case.
        let t = toks("nop # tail");
        assert_eq!(t[1], (TokKind::Comment, "# tail"));
    }

    #[test]
    fn label_only_and_with_instruction() {
        assert_eq!(toks("main:"), vec![(TokKind::Label, "main"), (TokKind::Punct, ":")]);
        assert_eq!(
            toks("main: ret"),
            vec![(TokKind::Label, "main"), (TokKind::Punct, ":"), (TokKind::Mnemonic, "ret")]
        );
    }

    #[test]
    fn directive_and_global_name() {
        assert_eq!(
            toks(".globl main"),
            vec![(TokKind::Directive, ".globl"), (TokKind::Ident, "main")]
        );
    }

    #[test]
    fn registers_all_classes() {
        for r in ["rax", "eax", "ax", "al", "r15d", "xmm0", "ymm15", "zmm31", "rip"] {
            let line = format!("push {r}");
            let t = lex_line(&line);
            assert_eq!(t[1].kind, TokKind::Register, "{r} should be a register");
        }
    }

    #[test]
    fn invoke_is_a_keyword_and_func_is_ident() {
        assert_eq!(
            toks("invoke ExitProcess, 7"),
            vec![
                (TokKind::Keyword, "invoke"),
                (TokKind::Ident, "ExitProcess"),
                (TokKind::Punct, ","),
                (TokKind::Number, "7"),
            ]
        );
    }

    #[test]
    fn upper_snake_operand_is_constant() {
        let t = toks("mov eax, OPEN_EXISTING");
        assert_eq!(t[3], (TokKind::Constant, "OPEN_EXISTING"));
    }

    #[test]
    fn struct_field_is_ident_not_constant() {
        // Mixed case (RECT.right) is not UPPER_SNAKE, so it's a plain ident.
        let t = toks("mov eax, [rcx + RECT.right]");
        assert!(t.iter().any(|&(k, s)| k == TokKind::Ident && s == "RECT.right"), "{t:?}");
        assert!(t.iter().any(|&(k, s)| k == TokKind::Register && s == "rcx"));
    }

    #[test]
    fn hex_and_sizeof_and_string() {
        assert_eq!(toks("mov rax, 0x1F")[3], (TokKind::Number, "0x1F"));
        let s = toks("invoke ExitProcess, sizeof(RECT)");
        assert!(s.iter().any(|&(k, t)| k == TokKind::Keyword && t == "sizeof"));
        assert!(s.iter().any(|&(k, t)| k == TokKind::Constant && t == "RECT"));
        assert_eq!(toks(".asciz \"hi\"")[1], (TokKind::String, "\"hi\""));
    }

    #[test]
    fn size_prefixed_memory() {
        let t = toks("mov qword ptr [rax], 1");
        assert_eq!(t[1], (TokKind::Keyword, "qword"));
        assert_eq!(t[2], (TokKind::Keyword, "ptr"));
        assert_eq!(t[3], (TokKind::Punct, "["));
        assert_eq!(t[4], (TokKind::Register, "rax"));
    }

    #[test]
    fn spans_are_exact_byte_offsets() {
        let line = "mov rax, 42";
        for t in lex_line(line) {
            assert!(t.start < t.end && t.end <= line.len());
        }
        // The register token slices back to exactly "rax".
        let reg = lex_line(line).into_iter().find(|t| t.kind == TokKind::Register).unwrap();
        assert_eq!(&line[reg.start..reg.end], "rax");
    }

    #[test]
    fn blank_and_whitespace_lines_have_no_tokens() {
        assert!(lex_line("").is_empty());
        assert!(lex_line("    \t  ").is_empty());
    }

    #[test]
    fn whole_buffer_lexes_per_line() {
        let rows = lex("main:\n  mov rax, 1\n  ret\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0].kind, TokKind::Label);
        assert_eq!(rows[1][0].kind, TokKind::Mnemonic);
        assert_eq!(rows[2][0].kind, TokKind::Mnemonic);
    }
}
