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

    // ── .rdata layout (import directory) ─────────────────────────────────────
    let rdata_rva = align_up(TEXT_RVA + text_vsize as u32, SECTION_ALIGN);
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
    let iat_slot_rva = |i: usize| iat_rva + (i * 8) as u32;
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
    for i in 0..flat.len() {
        // jmp qword ptr [rip + disp32] -> IAT slot; rip = thunk_rva(i)+6
        let disp = iat_slot_rva(i) as i64 - (thunk_rva(i) as i64 + THUNK as i64);
        text.push(0xFF);
        text.push(0x25);
        text.extend_from_slice(&(disp as i32).to_le_bytes());
    }
    for r in &module.relocs {
        match r.kind {
            RelocKind::BranchRel32 | RelocKind::RipRel32 => {
                let i = *func_index
                    .get(r.target.as_str())
                    .ok_or_else(|| anyhow::anyhow!("reloc to unknown import '{}'", r.target))?;
                let site_rva = TEXT_RVA as i64 + r.at as i64;
                let disp = thunk_rva(i) as i64 - (site_rva + 4);
                text[r.at..r.at + 4].copy_from_slice(&(disp as i32).to_le_bytes());
            }
            RelocKind::Abs64 => bail!("abs64 reloc to '{}' unsupported in PE writer", r.target),
        }
    }

    // ── section + image sizes ────────────────────────────────────────────────
    let text_raw = align_up(text.len() as u32, FILE_ALIGN);
    let rdata_file = HEADERS_FILE + text_raw;
    let rdata_raw = align_up(rdata.len() as u32, FILE_ALIGN);
    let size_of_image = align_up(rdata_rva + rdata_vsize as u32, SECTION_ALIGN);
    let entry_rva = TEXT_RVA + entry_off as u32;

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
    out.extend_from_slice(&2u16.to_le_bytes()); // NumberOfSections
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
    out.extend_from_slice(&rdata_raw.to_le_bytes()); // SizeOfInitializedData
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
    let mut sec = |out: &mut Vec<u8>, name: &[u8], vsize: u32, rva: u32, raw: u32, foff: u32, ch: u32| {
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
    sec(&mut out, b".text", text_vsize as u32, TEXT_RVA, text_raw, HEADERS_FILE, 0x6000_0020);
    sec(&mut out, b".rdata", rdata_vsize as u32, rdata_rva, rdata_raw, rdata_file, 0x4000_0040);

    // pad headers to SizeOfHeaders
    out.resize(HEADERS_FILE as usize, 0);
    // .text (padded)
    out.extend_from_slice(&text);
    out.resize((HEADERS_FILE + text_raw) as usize, 0);
    // .rdata (padded)
    out.extend_from_slice(&rdata);
    out.resize((rdata_file + rdata_raw) as usize, 0);

    Ok(out)
}
