//! complete.rs — autocomplete context detection (pure).
//!
//! Given the line being edited and the caret byte offset, decide *what* kind of
//! completion is wanted and the prefix to match — purely from the text, so it's
//! instant and unit-testable. The language thread turns a [`CompletionContext`]
//! into actual candidates via winkb (`Kb::complete` / `Kb::layout`).

/// What kind of name the caret is positioned to complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    /// The function name after `invoke`.
    Function,
    /// `Type.field` — a struct field.
    Field { type_name: String },
    /// A generic operand identifier (function / constant / type).
    Symbol,
    /// Nothing useful here (e.g. typing the mnemonic — no mnemonic list).
    None,
}

/// A resolved completion request: replace `line[start..cursor]` with a choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionContext {
    pub kind: CompletionKind,
    pub prefix: String,
    pub start: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c == '$' || c == '@'
}

/// The word ending at `cursor`: `(start, text)`.
fn word_before(line: &str, cursor: usize) -> (usize, &str) {
    let mut ws = cursor;
    while ws > 0 {
        let c = line[..ws].chars().next_back().unwrap();
        if is_word_char(c) {
            ws -= c.len_utf8();
        } else {
            break;
        }
    }
    (ws, &line[ws..cursor])
}

fn first_non_ws(line: &str) -> usize {
    line.char_indices().find(|(_, c)| !c.is_whitespace()).map(|(i, _)| i).unwrap_or(line.len())
}

/// The next word-char run at or after `from`: `(start, end)`.
pub(crate) fn next_word(line: &str, from: usize) -> Option<(usize, usize)> {
    let mut i = from.min(line.len());
    while i < line.len() {
        let c = line[i..].chars().next().unwrap();
        if is_word_char(c) {
            break;
        }
        i += c.len_utf8();
    }
    if i >= line.len() {
        return None;
    }
    let start = i;
    while i < line.len() {
        let c = line[i..].chars().next().unwrap();
        if is_word_char(c) {
            i += c.len_utf8();
        } else {
            break;
        }
    }
    Some((start, i))
}

/// The head word of the line (the mnemonic / `invoke` / directive), skipping an
/// optional leading `label:`.
pub(crate) fn head_word(line: &str) -> Option<(usize, usize)> {
    let i = first_non_ws(line);
    let (ws, we) = next_word(line, i)?;
    let rest = &line[we..];
    let trimmed = rest.trim_start();
    if trimmed.starts_with(':') {
        let colon = we + (rest.len() - trimmed.len());
        return next_word(line, colon + 1);
    }
    Some((ws, we))
}

/// Count top-level commas in `s` (`[]` depth aware) — the argument separator
/// count, used to find which `invoke` argument the caret is in.
pub(crate) fn count_top_level_commas(s: &str) -> usize {
    let mut depth = 0i32;
    let mut n = 0;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => n += 1,
            _ => {}
        }
    }
    n
}

/// The first top-level comma at/after `from` (`[]` depth aware).
fn top_level_comma(line: &str, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from.min(line.len());
    while i < line.len() {
        let c = line[i..].chars().next().unwrap();
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
        i += c.len_utf8();
    }
    None
}

/// Determine the completion context for `line` at byte offset `cursor`.
pub fn context(line: &str, cursor: usize) -> CompletionContext {
    let cursor = cursor.min(line.len());
    let (ws, word) = word_before(line, cursor);

    // `Type.field` takes precedence — function/symbol names never contain a dot.
    if let Some(dot) = word.rfind('.') {
        let type_name = word[..dot].to_string();
        if !type_name.is_empty() {
            return CompletionContext {
                kind: CompletionKind::Field { type_name },
                prefix: word[dot + 1..].to_string(),
                start: ws + dot + 1,
            };
        }
    }

    if let Some((hs, he)) = head_word(line) {
        // Typing the head word itself — no mnemonic list, so nothing to offer.
        if ws == hs {
            return CompletionContext {
                kind: CompletionKind::None,
                prefix: word.to_string(),
                start: ws,
            };
        }
        // After `invoke`, in the first operand → a function name.
        if line[hs..he].eq_ignore_ascii_case("invoke") && cursor > he {
            let in_first_arg = top_level_comma(line, he).map_or(true, |c| cursor <= c);
            if in_first_arg {
                return CompletionContext {
                    kind: CompletionKind::Function,
                    prefix: word.to_string(),
                    start: ws,
                };
            }
        }
    }

    if word.is_empty() {
        return CompletionContext { kind: CompletionKind::None, prefix: String::new(), start: ws };
    }
    CompletionContext { kind: CompletionKind::Symbol, prefix: word.to_string(), start: ws }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_after_invoke() {
        let line = "invoke Crea";
        let c = context(line, line.len());
        assert_eq!(c.kind, CompletionKind::Function);
        assert_eq!(c.prefix, "Crea");
        assert_eq!(&line[c.start..line.len()], "Crea");
    }

    #[test]
    fn function_with_empty_prefix_right_after_invoke() {
        let c = context("invoke ", 7);
        assert_eq!(c.kind, CompletionKind::Function);
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn second_operand_is_a_symbol_not_a_function() {
        let line = "invoke ExitProcess, OPEN";
        let c = context(line, line.len());
        assert_eq!(c.kind, CompletionKind::Symbol);
        assert_eq!(c.prefix, "OPEN");
    }

    #[test]
    fn struct_field_after_dot() {
        let line = "mov eax, [rcx + RECT.ri";
        let c = context(line, line.len());
        assert_eq!(c.kind, CompletionKind::Field { type_name: "RECT".to_string() });
        assert_eq!(c.prefix, "ri");
        assert_eq!(&line[c.start..line.len()], "ri");
    }

    #[test]
    fn field_with_empty_prefix_right_after_dot() {
        let line = "mov eax, [rcx + RECT.";
        let c = context(line, line.len());
        assert_eq!(c.kind, CompletionKind::Field { type_name: "RECT".to_string() });
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn typing_the_mnemonic_offers_nothing() {
        assert_eq!(context("mo", 2).kind, CompletionKind::None);
    }

    #[test]
    fn invoke_after_a_label() {
        let line = "main: invoke Crea";
        let c = context(line, line.len());
        assert_eq!(c.kind, CompletionKind::Function);
        assert_eq!(c.prefix, "Crea");
    }

    #[test]
    fn empty_line_offers_nothing() {
        assert_eq!(context("", 0).kind, CompletionKind::None);
    }

    #[test]
    fn generic_operand_is_a_symbol() {
        let line = "mov rax, GetSt";
        let c = context(line, line.len());
        assert_eq!(c.kind, CompletionKind::Symbol);
        assert_eq!(c.prefix, "GetSt");
    }
}
