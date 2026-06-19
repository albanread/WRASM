//! RASM — a from-scratch x86-64 machine-code encoder. Intel-syntax assembly
//! text in, machine code + symbols + relocations out. No LLVM, no JIT.
//!
//! * [`rasm`] — the encoder: parse → encode → two-pass assemble.
//! * [`backend`] — the [`Encoder`] trait and its [`EncodedModule`]/[`Reloc`] types.
//! * [`difftest`] — the differential driver plus the frozen-corpus regression
//!   gate (`corpus/x86_64.tsv`, 5109 forms) that keeps the encoder byte-identical
//!   to LLVM-MC without depending on LLVM.
//!
//! ```
//! use rasm::{RasmEncoder, Encoder};
//! let m = RasmEncoder.encode("mov rax, 42\nret\n").unwrap();
//! assert_eq!(m.code, vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3]);
//! ```

pub mod backend;
pub mod coff;
pub mod difftest;
pub mod rasm;

pub use backend::{EncodedModule, Encoder, Reloc, RelocKind};
pub use coff::write_coff;
pub use rasm::{assemble, RasmEncoder};
