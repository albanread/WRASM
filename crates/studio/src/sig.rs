//! sig.rs — signature help: which `invoke` parameter the caret is in (pure).
//!
//! `invoke F, a, b` calls `F` with the function name as the head argument; the
//! parameters are what follow. So the active parameter index is "top-level
//! commas before the caret, minus one". The language thread (`Request::Signature`)
//! turns `(function, active)` into the rendered signature with that param marked.

use crate::complete::{count_top_level_commas, head_word, next_word};

/// If `line` is an `invoke F, …` and the caret is in the argument list (past the
/// function name and after at least one comma), return `(F, active_param_index)`.
pub fn active_param(line: &str, cursor: usize) -> Option<(String, usize)> {
    let cursor = cursor.min(line.len());
    let (hs, he) = head_word(line)?;
    if !line[hs..he].eq_ignore_ascii_case("invoke") {
        return None;
    }
    let (fs, fe) = next_word(line, he)?;
    if cursor <= fe {
        return None; // still on/before the function name
    }
    let commas = count_top_level_commas(&line[..cursor]);
    if commas == 0 {
        return None; // typed the name but not yet a separating comma
    }
    Some((line[fs..fe].to_string(), commas - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_param_after_first_comma() {
        let line = "invoke CreateFileW, rcx";
        assert_eq!(active_param(line, line.len()), Some(("CreateFileW".to_string(), 0)));
    }

    #[test]
    fn third_param_after_two_commas() {
        let line = "invoke CreateFileW, a, b, c";
        assert_eq!(active_param(line, line.len()), Some(("CreateFileW".to_string(), 2)));
    }

    #[test]
    fn on_the_function_name_is_none() {
        let line = "invoke CreateFil";
        assert_eq!(active_param(line, line.len()), None);
    }

    #[test]
    fn brackets_do_not_count_as_separators() {
        // The comma inside [rax + rbx*2] must not advance the parameter index.
        let line = "invoke Foo, [rax + rbx], next";
        // cursor right after the bracketed arg's comma -> second param (index 1).
        assert_eq!(active_param(line, line.len()), Some(("Foo".to_string(), 1)));
    }

    #[test]
    fn non_invoke_line_is_none() {
        assert_eq!(active_param("mov rax, 1", 9), None);
    }
}
