//! Self-contained PE (x86-64 .exe) writer: [`EncodedModule`] + an import map →
//! a runnable executable, no external linker.
//!
//! Imports are wired without changing rasm's output: each imported function gets
//! a 6-byte thunk `jmp [rip+__imp_Name]` in `.text`, and rasm's direct
//! `call Name` (a `BranchRel32` reloc) is patched to reach that thunk. The thunk
//! jumps through the Import Address Table slot the loader fills in.
//!
//! Layout: headers (1 page) · `.text` (code + thunks) · `.rdata` (import table).
//! Fixed image base, RIP-relative code, IAT-based calls → no `.reloc` needed.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::backend::{EncodedModule, RelocKind};

const IMAGE_BASE: u64 = 0x1_4000_0000;
const SECTION_ALIGN: u32 = 0x1000;
const FILE_ALIGN: u32 = 0x200;
const TEXT_RVA: u32 = 0x1000;
const HEADERS_FILE: u32 = 0x200; // headers fit in one file-alignment unit

fn align_up(x: u32, a: u32) -> u32 {
    (x + a - 1) / a * a
}

/// Build a PE executable. `imports` maps each name in `module.externs` to its
/// DLL; `entry` must be a `.globl` symbol in `module.symbols`.
pub fn write_pe(
    module: &EncodedModule,
    imports: &BTreeMap<String, String>,
    entry: &str,
) -> Result<Vec<u8>> {
    let entry_off = *module
        .symbols
        .get(entry)
        .ok_or_else(|| anyhow::anyhow!("entry symbol '{entry}' not defined (.globl it)"))?;

    // ── group imports by DLL, assign a stable order ──────────────────────────
    let mut by_dll: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &module.externs {
        let dll = imports
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no DLL known for import '{name}'"))?;
        by_dll.entry(dll.clone()).or_default().push(name.clone());
    }
    // Flat import order = the order thunks/IAT slots are laid out in.
    let flat: Vec<(String, String)> = by_dll
        .iter()
        .flat_map(|(dll, fns)| fns.iter().map(move |f| (f.clone(), dll.clone())))
        .collect();
    let func_index: BTreeMap<&str, usize> =
        flat.iter().enumerate().map(|(i, (f, _))| (f.as_str(), i)).collect();

    // ── .text = code + one 6-byte thunk per import ───────────────────────────
    const THUNK: usize = 6; // FF 25 <disp32>
    let code_len = module.code.len();
    let thunks_rva = TEXT_RVA + code_len as u32;
    let thunk_rva = |i: usize| thunks_rva + (i * THUNK) as u32;
    let text_vsize = code_len + flat.len() * THUNK;

    // ── .data layout (read/write globals) ────────────────────────────────────
    let has_data = !module.data.is_empty();
    let data_rva = align_up(TEXT_RVA + text_vsize as u32, SECTION_ALIGN);
    let data_vsize = module.data.len() as u32;
    let after_data = if has_data { data_rva + data_vsize } else { TEXT_RVA + text_vsize as u32 };

    // ── .rdata layout (import directory) ─────────────────────────────────────
    let rdata_rva = align_up(after_data, SECTION_ALIGN);
    let ndll = by_dll.len();
    let nfunc = flat.len();
    let descs_off = 0usize;
    let descs_size = 20 * (ndll + 1);
    let ilt_off = descs_off + descs_size;
    let ilt_size = 8 * (nfunc + ndll); // funcs + null terminator per DLL
    let iat_off = ilt_off + ilt_size;
    let iat_size = ilt_size;
    let names_off = iat_off + iat_size;

    // Hint/name entries + DLL name strings, recording their RVAs.
    let mut names = Vec::<u8>::new();
    let mut hintname_rva: BTreeMap<&str, u32> = BTreeMap::new();
    for (f, _) in &flat {
        let rva = rdata_rva + (names_off + names.len()) as u32;
        hintname_rva.insert(f.as_str(), rva);
        names.extend_from_slice(&0u16.to_le_bytes()); // hint
        names.extend_from_slice(f.as_bytes());
        names.push(0);
        if names.len() % 2 != 0 {
            names.push(0);
        }
    }
    let mut dllname_rva: BTreeMap<&str, u32> = BTreeMap::new();
    for dll in by_dll.keys() {
        let rva = rdata_rva + (names_off + names.len()) as u32;
        dllname_rva.insert(dll.as_str(), rva);
        names.extend_from_slice(dll.as_bytes());
        names.push(0);
    }

    let iat_rva = rdata_rva + iat_off as u32;
    // The IAT stores each DLL's entries followed by a null terminator, so a
    // function's slot index is NOT its flat index once there's more than one DLL.
    // Map every function to its real slot so its thunk points at the right entry.
    let mut iat_slot_of: BTreeMap<String, usize> = BTreeMap::new();
    {
        let mut s = 0usize;
        for fns in by_dll.values() {
            for f in fns {
                iat_slot_of.insert(f.clone(), s);
                s += 1;
            }
            s += 1; // null terminator
        }
    }
    let rdata_vsize = names_off + names.len();

    // ── assemble .rdata bytes ────────────────────────────────────────────────
    let mut rdata = vec![0u8; names_off];
    // import descriptors
    let mut d = descs_off;
    let mut slot = 0usize; // running index into ILT/IAT
    for (dll, fns) in &by_dll {
        // OriginalFirstThunk (ILT), TimeDateStamp, ForwarderChain, Name, FirstThunk (IAT)
        let ilt_this = rdata_rva + (ilt_off + slot * 8) as u32;
        let iat_this = rdata_rva + (iat_off + slot * 8) as u32;
        rdata[d..d + 4].copy_from_slice(&ilt_this.to_le_bytes());
        rdata[d + 12..d + 16].copy_from_slice(&dllname_rva[dll.as_str()].to_le_bytes());
        rdata[d + 16..d + 20].copy_from_slice(&iat_this.to_le_bytes());
        d += 20;
        slot += fns.len() + 1; // + null terminator
    }
    // ILT and IAT entries (identical pre-load: RVA of hint/name)
    let mut s = 0usize;
    for (_dll, fns) in &by_dll {
        for f in fns {
            let v = hintname_rva[f.as_str()] as u64;
            let il = ilt_off + s * 8;
            let ia = iat_off + s * 8;
            rdata[il..il + 8].copy_from_slice(&v.to_le_bytes());
            rdata[ia..ia + 8].copy_from_slice(&v.to_le_bytes());
            s += 1;
        }
        s += 1; // null terminator slot (already zero)
    }
    rdata.extend_from_slice(&names);

    // ── assemble .text bytes (code + thunks, relocs patched) ─────────────────
    let mut text = module.code.clone();
    for (i, (f, _)) in flat.iter().enumerate() {
        // jmp qword ptr [rip + disp32] -> this function's IAT slot; rip = thunk_rva(i)+6
        let slot_rva = iat_rva + (iat_slot_of[f] * 8) as u32;
        let disp = slot_rva as i64 - (thunk_rva(i) as i64 + THUNK as i64);
        text.push(0xFF);
        text.push(0x25);
        text.extend_from_slice(&(disp as i32).to_le_bytes());
    }
    for r in &module.relocs {
        // A reference into our own `.data` (a global) resolves against the data
        // section's address — no import thunk involved.
        if let Some(&doff) = module.data_symbols.get(r.target.as_str()) {
            let site_rva = TEXT_RVA as i64 + r.at as i64;
            // The disp32 field already holds the operand's literal `+disp` (the
            // `[rip + sym + disp]` offset); fold it in rather than overwrite it.
            let field = i32::from_le_bytes(text[r.at..r.at + 4].try_into().unwrap()) as i64;
            let target_rva = data_rva as i64 + doff as i64 + r.addend;
            let disp = target_rva - (site_rva + 4) + field;
            text[r.at..r.at + 4].copy_from_slice(&(disp as i32).to_le_bytes());
            continue;
        }
        match r.kind {
            RelocKind::BranchRel32 | RelocKind::RipRel32 => {
                let i = *func_index
                    .get(r.target.as_str())
                    .ok_or_else(|| anyhow::anyhow!("reloc to unknown import '{}'", r.target))?;
                let site_rva = TEXT_RVA as i64 + r.at as i64;
                // Fold the disp32 field (a `[rip+import+disp]` offset; 0 for branches).
                let field = i32::from_le_bytes(text[r.at..r.at + 4].try_into().unwrap()) as i64;
                let disp = thunk_rva(i) as i64 - (site_rva + 4) + field;
                text[r.at..r.at + 4].copy_from_slice(&(disp as i32).to_le_bytes());
            }
            RelocKind::Abs64 => bail!("abs64 reloc to '{}' unsupported in PE writer", r.target),
        }
    }

    // ── section + image sizes ────────────────────────────────────────────────
    let text_raw = align_up(text.len() as u32, FILE_ALIGN);
    let data_raw = if has_data { align_up(data_vsize, FILE_ALIGN) } else { 0 };
    let data_file = HEADERS_FILE + text_raw;
    let rdata_file = data_file + data_raw;
    let rdata_raw = align_up(rdata.len() as u32, FILE_ALIGN);
    let size_of_image = align_up(rdata_rva + rdata_vsize as u32, SECTION_ALIGN);
    let entry_rva = TEXT_RVA + entry_off as u32;
    let nsections: u16 = if has_data { 3 } else { 2 };

    // ── headers ──────────────────────────────────────────────────────────────
    let mut out = Vec::<u8>::new();
    // DOS header (64 bytes): MZ, e_lfanew=0x40, no stub.
    out.extend_from_slice(b"MZ");
    out.resize(0x3C, 0);
    out.extend_from_slice(&0x40u32.to_le_bytes());
    out.resize(0x40, 0);
    // PE signature + COFF file header
    out.extend_from_slice(b"PE\0\0");
    out.extend_from_slice(&0x8664u16.to_le_bytes()); // Machine = AMD64
    out.extend_from_slice(&nsections.to_le_bytes()); // NumberOfSections
    out.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToSymbolTable
    out.extend_from_slice(&0u32.to_le_bytes()); // NumberOfSymbols
    out.extend_from_slice(&240u16.to_le_bytes()); // SizeOfOptionalHeader (PE32+, 16 dirs)
    out.extend_from_slice(&0x0022u16.to_le_bytes()); // EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE

    // Optional header (PE32+)
    out.extend_from_slice(&0x20bu16.to_le_bytes()); // Magic PE32+
    out.push(14); // MajorLinkerVersion
    out.push(0); // MinorLinkerVersion
    out.extend_from_slice(&text_raw.to_le_bytes()); // SizeOfCode
    out.extend_from_slice(&(rdata_raw + data_raw).to_le_bytes()); // SizeOfInitializedData
    out.extend_from_slice(&0u32.to_le_bytes()); // SizeOfUninitializedData
    out.extend_from_slice(&entry_rva.to_le_bytes()); // AddressOfEntryPoint
    out.extend_from_slice(&TEXT_RVA.to_le_bytes()); // BaseOfCode
    out.extend_from_slice(&IMAGE_BASE.to_le_bytes()); // ImageBase
    out.extend_from_slice(&SECTION_ALIGN.to_le_bytes());
    out.extend_from_slice(&FILE_ALIGN.to_le_bytes());
    out.extend_from_slice(&6u16.to_le_bytes()); // MajorOSVersion
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // MajorImageVersion
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&6u16.to_le_bytes()); // MajorSubsystemVersion
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // Win32VersionValue
    out.extend_from_slice(&size_of_image.to_le_bytes());
    out.extend_from_slice(&HEADERS_FILE.to_le_bytes()); // SizeOfHeaders
    out.extend_from_slice(&0u32.to_le_bytes()); // CheckSum
    out.extend_from_slice(&3u16.to_le_bytes()); // Subsystem = CONSOLE
    out.extend_from_slice(&0x100u16.to_le_bytes()); // DllCharacteristics = NX_COMPAT
    out.extend_from_slice(&0x100000u64.to_le_bytes()); // SizeOfStackReserve
    out.extend_from_slice(&0x1000u64.to_le_bytes()); // SizeOfStackCommit
    out.extend_from_slice(&0x100000u64.to_le_bytes()); // SizeOfHeapReserve
    out.extend_from_slice(&0x1000u64.to_le_bytes()); // SizeOfHeapCommit
    out.extend_from_slice(&0u32.to_le_bytes()); // LoaderFlags
    out.extend_from_slice(&16u32.to_le_bytes()); // NumberOfRvaAndSizes
    // Data directories (16). [1] = Import, [12] = IAT.
    let mut dirs = [[0u32; 2]; 16];
    dirs[1] = [rdata_rva + descs_off as u32, descs_size as u32];
    dirs[12] = [iat_rva, iat_size as u32];
    for [rva, size] in dirs {
        out.extend_from_slice(&rva.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }

    // Section headers
    let sec = |out: &mut Vec<u8>, name: &[u8], vsize: u32, rva: u32, raw: u32, foff: u32, ch: u32| {
        let mut n = [0u8; 8];
        n[..name.len()].copy_from_slice(name);
        out.extend_from_slice(&n);
        out.extend_from_slice(&vsize.to_le_bytes());
        out.extend_from_slice(&rva.to_le_bytes());
        out.extend_from_slice(&raw.to_le_bytes());
        out.extend_from_slice(&foff.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // PointerToRelocations
        out.extend_from_slice(&0u32.to_le_bytes()); // PointerToLinenumbers
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&ch.to_le_bytes());
    };
    // .text (RX), then .data (RW, INITIALIZED|READ|WRITE) if any, then .rdata (R).
    sec(&mut out, b".text", text_vsize as u32, TEXT_RVA, text_raw, HEADERS_FILE, 0x6000_0020);
    if has_data {
        sec(&mut out, b".data", data_vsize, data_rva, data_raw, data_file, 0xC000_0040);
    }
    sec(&mut out, b".rdata", rdata_vsize as u32, rdata_rva, rdata_raw, rdata_file, 0x4000_0040);

    // pad headers to SizeOfHeaders
    out.resize(HEADERS_FILE as usize, 0);
    // .text (padded)
    out.extend_from_slice(&text);
    out.resize((HEADERS_FILE + text_raw) as usize, 0);
    // .data (padded)
    if has_data {
        out.extend_from_slice(&module.data);
        out.resize((data_file + data_raw) as usize, 0);
    }
    // .rdata (padded)
    out.extend_from_slice(&rdata);
    out.resize((rdata_file + rdata_raw) as usize, 0);

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Reloc, RelocKind};

    /// `lea rcx, [rip + counter]; ret`, where `counter` is a 4-byte `.data` global.
    fn module_with_data() -> EncodedModule {
        EncodedModule {
            code: vec![0x48, 0x8D, 0x0D, 0, 0, 0, 0, 0xC3],
            data: vec![0, 0, 0, 0],
            symbols: BTreeMap::from([("main".to_string(), 0usize)]),
            data_symbols: BTreeMap::from([("counter".to_string(), 0usize)]),
            relocs: vec![Reloc {
                at: 3,
                size: 4,
                kind: RelocKind::RipRel32,
                target: "counter".to_string(),
                addend: 0,
            }],
            externs: vec![],
        }
    }

    fn num_sections(exe: &[u8]) -> u16 {
        u16::from_le_bytes([exe[0x46], exe[0x47]]) // e_lfanew=0x40, +4 sig +2 machine
    }

    #[test]
    fn writable_data_section_emitted_and_cross_ref_resolved() {
        let exe = write_pe(&module_with_data(), &BTreeMap::new(), "main").unwrap();
        assert_eq!(&exe[0..2], b"MZ");
        assert_eq!(num_sections(&exe), 3, "headers + .text + .data + .rdata");
        assert!(exe.windows(5).any(|w| w == b".data"), ".data section header present");
        // The code→data RIP-rel field (code off 3 → file 0x203) was patched.
        assert_ne!(&exe[0x203..0x207], &[0, 0, 0, 0], "code→data disp resolved, not a placeholder");
    }

    #[test]
    fn multi_dll_thunks_skip_the_iat_null_terminators() {
        // Two imports in two different DLLs. Each DLL's IAT run ends in a null
        // slot, so the second DLL's function lives at slot index 2, not 1 — its
        // thunk must skip the terminator. (Regression: thunks once used the flat
        // function index and pointed one slot short.)
        let m = EncodedModule {
            code: vec![0xE8, 0, 0, 0, 0, 0xE8, 0, 0, 0, 0], // call AFunc ; call BFunc
            symbols: BTreeMap::from([("main".to_string(), 0usize)]),
            relocs: vec![
                Reloc { at: 1, size: 4, kind: RelocKind::BranchRel32, target: "AFunc".into(), addend: 0 },
                Reloc { at: 6, size: 4, kind: RelocKind::BranchRel32, target: "BFunc".into(), addend: 0 },
            ],
            externs: vec!["AFunc".into(), "BFunc".into()],
            ..Default::default()
        };
        let imports =
            BTreeMap::from([("AFunc".to_string(), "a.dll".to_string()), ("BFunc".to_string(), "b.dll".to_string())]);
        let exe = write_pe(&m, &imports, "main").unwrap();

        // Thunks follow the 10 code bytes in .text (file offset 0x200 + 10).
        let thunk0 = 0x200 + 10;
        let thunk1 = thunk0 + 6;
        let rip0 = 0x1000 + 10 + 6; // RVA just past thunk 0
        let rip1 = 0x1000 + 10 + 12;
        let disp0 = i32::from_le_bytes([exe[thunk0 + 2], exe[thunk0 + 3], exe[thunk0 + 4], exe[thunk0 + 5]]);
        let disp1 = i32::from_le_bytes([exe[thunk1 + 2], exe[thunk1 + 3], exe[thunk1 + 4], exe[thunk1 + 5]]);
        let target0 = rip0 as i64 + disp0 as i64; // IAT slot RVA for AFunc
        let target1 = rip1 as i64 + disp1 as i64; // IAT slot RVA for BFunc
        assert_eq!(target1 - target0, 16, "BFunc's IAT slot must be 2 slots after AFunc's (a null between)");
    }

    #[test]
    fn no_data_keeps_two_sections() {
        let m = EncodedModule {
            code: vec![0xC3], // ret
            symbols: BTreeMap::from([("main".to_string(), 0usize)]),
            ..Default::default()
        };
        let exe = write_pe(&m, &BTreeMap::new(), "main").unwrap();
        assert_eq!(num_sections(&exe), 2, "no .data → just .text + .rdata");
        assert!(!exe.windows(5).any(|w| w == b".data"));
    }
}
