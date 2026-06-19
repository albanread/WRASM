//! hover.rs — the token the caret rests on (pure).
//!
//! Finding *which* token is under the caret is pure lexing; turning it into a
//! tooltip (resolve a constant's value, a function's signature, a type's size)
//! is a winkb lookup on the language thread (`Request::Hover`).

use crate::syntax::{lex_line, Token};

/// The token covering byte offset `col` in `line`, if any.
pub fn token_at(line: &str, col: usize) -> Option<Token> {
    lex_line(line).into_iter().find(|t| t.start <= col && col < t.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::TokKind;

    #[test]
    fn finds_the_constant_under_the_caret() {
        let line = "mov eax, OPEN_EXISTING";
        let t = token_at(line, 12).expect("a token at col 12");
        assert_eq!(t.kind, TokKind::Constant);
        assert_eq!(&line[t.start..t.end], "OPEN_EXISTING");
    }

    #[test]
    fn finds_the_register() {
        let line = "mov rax, 1";
        let t = token_at(line, 4).unwrap();
        assert_eq!(t.kind, TokKind::Register);
        assert_eq!(&line[t.start..t.end], "rax");
    }

    #[test]
    fn gap_between_tokens_is_none() {
        // The space at col 3 belongs to no token.
        assert!(token_at("mov rax", 3).is_none());
    }
}
