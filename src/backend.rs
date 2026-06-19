//! The encoder contract and its data types.
//!
//! [`Encoder`] turns assembled (post-macro-expansion) Intel-syntax text into a
//! position-independent code blob plus a symbol table and relocation list. The
//! native [`RasmEncoder`](crate::rasm::RasmEncoder) implements it.

use std::collections::BTreeMap;

use anyhow::Result;

/// A relocation to resolve once final addresses are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reloc {
    /// Byte offset of the field to patch within the encoded code blob.
    pub at: usize,
    /// Field width in bytes (4 for rel32 / RIP-rel disp32, 8 for abs64).
    pub size: u8,
    pub kind: RelocKind,
    /// Target symbol name (internal label or extern).
    pub target: String,
    /// Constant added to the resolved target before encoding.
    pub addend: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// `call`/`jmp`/`jcc rel32`: field = target - (field_addr + 4).
    BranchRel32,
    /// `lea reg,[rip+disp32]` and friends: field = target - (field_addr + 4).
    RipRel32,
    /// 64-bit absolute address embedded in a data cell.
    Abs64,
}

/// The product of [`Encoder::encode`]: a position-independent code blob plus the
/// symbol table and relocation list a loader needs to place it.
#[derive(Debug, Clone, Default)]
pub struct EncodedModule {
    /// The encoded bytes, with reloc fields left as placeholders (0).
    pub code: Vec<u8>,
    /// `name -> byte offset` for every `.globl`/labelled symbol defined here.
    pub symbols: BTreeMap<String, usize>,
    /// Relocations to apply at load time.
    pub relocs: Vec<Reloc>,
    /// Names referenced but not defined here (externs to bind).
    pub externs: Vec<String>,
}

/// Encode assembled (post-macro-expansion) Intel-syntax assembly into machine
/// code. The input is the same text LLVM-MC is fed, which lets the encoder be a
/// drop-in and aids byte-identity.
pub trait Encoder {
    fn encode(&self, asm_text: &str) -> Result<EncodedModule>;
}
