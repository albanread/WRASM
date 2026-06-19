//! bytes.rs — formatting machine code for the live-bytes view (pure).
//!
//! The encoding itself happens on the language thread (`Request::LineBytes`,
//! which lowers one line through `was` and encodes it with `rasm`); this is just
//! the display side, kept pure and testable.

/// Space-separated lowercase hex, e.g. `[0x48, 0xc3]` -> `"48 c3"`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(hex(&[0x48, 0xc7, 0xc0, 0x2a, 0, 0, 0]), "48 c7 c0 2a 00 00 00");
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(hex(&[]), "");
    }
}
