//! COFF (x86-64) object writer: [`EncodedModule`] → `.obj` bytes that
//! `lld-link` / `link.exe` can link into an executable.
//!
//! A `.text` (code) section and, when the module has globals, a `.data`
//! (read/write) section. The symbol table holds the `.globl` code definitions
//! (external), the `.data` labels (static, so internal `.text`→`.data`
//! references resolve), and the undefined externs every relocation targets, plus
//! one COFF relocation per rasm [`Reloc`]. Direct `call <import>` works: the
//! linker turns an undefined external referenced by a `REL32` into a call thunk
//! through the import address table.

use std::collections::BTreeMap;

use crate::backend::{EncodedModule, RelocKind};

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_SCN_CNT_CODE: u32 = 0x0000_0020;
const IMAGE_SCN_CNT_INITIALIZED_DATA: u32 = 0x0000_0040;
const IMAGE_SCN_ALIGN_16BYTES: u32 = 0x0050_0000;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;
const IMAGE_SYM_CLASS_EXTERNAL: u8 = 2;
const IMAGE_SYM_CLASS_STATIC: u8 = 3;
const IMAGE_SYM_DTYPE_FUNCTION: u16 = 0x20; // DTYPE 2 in the high nibble
const IMAGE_SYM_DTYPE_NULL: u16 = 0x00; // plain data
const IMAGE_REL_AMD64_ADDR64: u16 = 0x0001;
const IMAGE_REL_AMD64_REL32: u16 = 0x0004;

const HEADER_SIZE: usize = 20;
const SECTION_HEADER_SIZE: usize = 40;
const RELOC_SIZE: usize = 10;

struct Sym {
    name: String,
    /// Offset within its section for a definition; 0 for an undefined extern.
    value: u32,
    /// 1 = `.text`, 2 = `.data`, 0 = undefined (IMAGE_SYM_UNDEFINED).
    section: i16,
    dtype: u16,
    class: u8,
}

/// Serialize `m` into a COFF object file.
pub fn write_coff(m: &EncodedModule) -> Vec<u8> {
    let has_data = !m.data.is_empty();
    let data_section: i16 = 2; // `.data` is section 2 when present
    let nsections: u16 = if has_data { 2 } else { 1 };

    // ── symbol table: defined `.text` globls, defined `.data` labels, then the
    //    undefined externs every relocation needs to name ─────────────────────
    let mut syms: Vec<Sym> = Vec::new();
    let mut index: BTreeMap<String, u32> = BTreeMap::new();
    for (name, &off) in &m.symbols {
        index.insert(name.clone(), syms.len() as u32);
        syms.push(Sym {
            name: name.clone(),
            value: off as u32,
            section: 1,
            dtype: IMAGE_SYM_DTYPE_FUNCTION,
            class: IMAGE_SYM_CLASS_EXTERNAL,
        });
    }
    // Data labels are module-local statics — enough for an internal `.text`→
    // `.data` reference to resolve at link time.
    for (name, &off) in &m.data_symbols {
        index.entry(name.clone()).or_insert_with(|| {
            let i = syms.len() as u32;
            syms.push(Sym {
                name: name.clone(),
                value: off as u32,
                section: data_section,
                dtype: IMAGE_SYM_DTYPE_NULL,
                class: IMAGE_SYM_CLASS_STATIC,
            });
            i
        });
    }
    let ensure_undef = |name: &str, syms: &mut Vec<Sym>, index: &mut BTreeMap<String, u32>| {
        if !index.contains_key(name) {
            index.insert(name.to_string(), syms.len() as u32);
            syms.push(Sym {
                name: name.to_string(),
                value: 0,
                section: 0,
                dtype: IMAGE_SYM_DTYPE_FUNCTION,
                class: IMAGE_SYM_CLASS_EXTERNAL,
            });
        }
    };
    for name in &m.externs {
        ensure_undef(name, &mut syms, &mut index);
    }
    for r in &m.relocs {
        ensure_undef(&r.target, &mut syms, &mut index);
    }

    let code_len = m.code.len();
    let data_len = m.data.len();
    let nreloc = m.relocs.len();
    // header · section headers · .text raw · .text relocs · .data raw · symtab
    let ptr_to_raw = HEADER_SIZE + nsections as usize * SECTION_HEADER_SIZE;
    let ptr_to_relocs = if nreloc > 0 { ptr_to_raw + code_len } else { 0 };
    let ptr_to_data = ptr_to_raw + code_len + nreloc * RELOC_SIZE;
    let ptr_to_symtab = ptr_to_data + if has_data { data_len } else { 0 };

    let mut out: Vec<u8> = Vec::new();

    // ── COFF file header ──────────────────────────────────────────────────────
    out.extend_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
    out.extend_from_slice(&nsections.to_le_bytes()); // NumberOfSections
    out.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp (deterministic)
    out.extend_from_slice(&(ptr_to_symtab as u32).to_le_bytes());
    out.extend_from_slice(&(syms.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // SizeOfOptionalHeader
    out.extend_from_slice(&0u16.to_le_bytes()); // Characteristics

    // ── .text section header ──────────────────────────────────────────────────
    let mut name = [0u8; 8];
    name[..5].copy_from_slice(b".text");
    out.extend_from_slice(&name);
    out.extend_from_slice(&0u32.to_le_bytes()); // VirtualSize (0 in objects)
    out.extend_from_slice(&0u32.to_le_bytes()); // VirtualAddress
    out.extend_from_slice(&(code_len as u32).to_le_bytes());
    out.extend_from_slice(&(ptr_to_raw as u32).to_le_bytes());
    out.extend_from_slice(&(ptr_to_relocs as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToLinenumbers
    out.extend_from_slice(&(nreloc as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfLinenumbers
    let characteristics =
        IMAGE_SCN_CNT_CODE | IMAGE_SCN_ALIGN_16BYTES | IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ;
    out.extend_from_slice(&characteristics.to_le_bytes());

    // ── .data section header (read/write globals) ─────────────────────────────
    if has_data {
        let mut dname = [0u8; 8];
        dname[..5].copy_from_slice(b".data");
        out.extend_from_slice(&dname);
        out.extend_from_slice(&0u32.to_le_bytes()); // VirtualSize
        out.extend_from_slice(&0u32.to_le_bytes()); // VirtualAddress
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        out.extend_from_slice(&(ptr_to_data as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // PointerToRelocations (none)
        out.extend_from_slice(&0u32.to_le_bytes()); // PointerToLinenumbers
        out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfRelocations
        out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfLinenumbers
        let dchar = IMAGE_SCN_CNT_INITIALIZED_DATA
            | IMAGE_SCN_ALIGN_16BYTES
            | IMAGE_SCN_MEM_READ
            | IMAGE_SCN_MEM_WRITE;
        out.extend_from_slice(&dchar.to_le_bytes());
    }

    // ── .text raw ─────────────────────────────────────────────────────────────
    out.extend_from_slice(&m.code);

    // ── relocations (all against `.text`) ─────────────────────────────────────
    for r in &m.relocs {
        let typ = match r.kind {
            RelocKind::BranchRel32 | RelocKind::RipRel32 => IMAGE_REL_AMD64_REL32,
            RelocKind::Abs64 => IMAGE_REL_AMD64_ADDR64,
        };
        out.extend_from_slice(&(r.at as u32).to_le_bytes()); // VirtualAddress
        out.extend_from_slice(&index[&r.target].to_le_bytes()); // SymbolTableIndex
        out.extend_from_slice(&typ.to_le_bytes());
    }

    // ── .data raw ─────────────────────────────────────────────────────────────
    if has_data {
        out.extend_from_slice(&m.data);
    }

    // ── symbol table (+ string table for names > 8 bytes) ─────────────────────
    let mut strtab: Vec<u8> = vec![0, 0, 0, 0]; // size patched in at the end
    for s in &syms {
        let bytes = s.name.as_bytes();
        if bytes.len() <= 8 {
            let mut field = [0u8; 8];
            field[..bytes.len()].copy_from_slice(bytes);
            out.extend_from_slice(&field);
        } else {
            let off = strtab.len() as u32;
            strtab.extend_from_slice(bytes);
            strtab.push(0);
            out.extend_from_slice(&[0, 0, 0, 0]); // zero -> name is in the string table
            out.extend_from_slice(&off.to_le_bytes());
        }
        out.extend_from_slice(&s.value.to_le_bytes());
        out.extend_from_slice(&s.section.to_le_bytes());
        out.extend_from_slice(&s.dtype.to_le_bytes());
        out.push(s.class);
        out.push(0); // NumberOfAuxSymbols
    }

    // ── string table ──────────────────────────────────────────────────────────
    let strtab_size = strtab.len() as u32;
    strtab[..4].copy_from_slice(&strtab_size.to_le_bytes());
    out.extend_from_slice(&strtab);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rasm::assemble;

    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }
    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn coff_header_section_and_symbols() {
        // A tiny program that calls an imported function -> one extern, one reloc.
        let m = assemble(".globl main\nmain:\n  sub rsp, 40\n  mov ecx, 42\n  call ExitProcess\n")
            .unwrap();
        let obj = write_coff(&m);

        assert_eq!(u16_at(&obj, 0), IMAGE_FILE_MACHINE_AMD64);
        assert_eq!(u16_at(&obj, 2), 1, "one section");
        assert_eq!(&obj[20..25], b".text");

        // .text raw data == rasm's code, at PointerToRawData.
        let raw_ptr = u32_at(&obj, 20 + 20) as usize;
        let raw_size = u32_at(&obj, 20 + 16) as usize;
        assert_eq!(&obj[raw_ptr..raw_ptr + raw_size], &m.code[..]);

        // One relocation, REL32, targeting ExitProcess.
        assert_eq!(u16_at(&obj, 20 + 32), 1, "one relocation");

        // Symbol table has main (defined) and ExitProcess (undefined extern).
        // Header: PointerToSymbolTable @8, NumberOfSymbols @12.
        let nsym = u32_at(&obj, 12) as usize;
        assert_eq!(nsym, 2);
        let sym_ptr = u32_at(&obj, 8) as usize;
        // main fits inline in the first symbol's name field.
        assert_eq!(&obj[sym_ptr..sym_ptr + 4], b"main");
        // ExitProcess (>8 chars) lives in the string table.
        let strtab_start = sym_ptr + nsym * 18; // 18 = COFF symbol record size
        let strtab = &obj[strtab_start..];
        assert!(
            strtab.windows(11).any(|w| w == b"ExitProcess"),
            "ExitProcess should be in the string table"
        );
    }

    #[test]
    fn coff_emits_data_section_with_defined_static_symbol() {
        // A `.data` global referenced from `.text`.
        let m = assemble(
            ".data\ncounter:\n.long 0\n.text\n.globl main\nmain:\nlea rcx, [rip + counter]\nret\n",
        )
        .unwrap();
        let obj = write_coff(&m);

        assert_eq!(u16_at(&obj, 2), 2, "two sections: .text + .data");
        assert_eq!(&obj[20 + 40..20 + 40 + 5], b".data", "second section is .data");

        // `counter` must be a DEFINED symbol in section 2 — not an undefined
        // external — or the link would fail to resolve the reference.
        let nsym = u32_at(&obj, 12) as usize;
        let sym_ptr = u32_at(&obj, 8) as usize;
        let counter = (0..nsym)
            .map(|i| &obj[sym_ptr + i * 18..sym_ptr + i * 18 + 18])
            .find(|rec| &rec[0..7] == b"counter")
            .expect("counter symbol present");
        assert_eq!(i16::from_le_bytes([counter[12], counter[13]]), 2, "counter defined in .data");
    }
}
