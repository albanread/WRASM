//! was — assemble a Windows source file to a COFF object, resolving `invoke`,
//! Windows constants, struct fields, and `sizeof` through winkb.
//!
//!   was input.asm -o output.obj
//!   was input.asm --emit-asm        # print the lowered rasm text and stop
//!
//! Knowledge DB: $WINKB_DB, else E:\windows_api\windows_api.db.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use rasm::{assemble, write_coff, write_pe};
use winkb::Kb;

const HELP: &str = "\
was — assemble WRASM (a Windows x86-64 assembly dialect) to a PE .exe or COFF object.

USAGE
  was <input.was> -o <out.exe|out.obj>   assemble (.exe = self-contained PE, else COFF)
  was <input.was> --emit-asm             print the lowered rasm text, then stop
  was <input.was> --check                semantic check only (diagnostics, no output)
  was <input.was> -o x.exe --entry NAME  set the PE entry symbol (default: main)
  was -h | --help                        this help

The knowledge DB ($WINKB_DB, else E:\\windows_api\\windows_api.db) resolves `invoke`
signatures, Windows constants, struct fields, and `sizeof`.

THE WRASM DIALECT — Intel/MASM-compatible, with exceptions
  Intel syntax (`mov dst, src`; `[rip + label]`) plus MASM-style macros: `invoke`,
  `proc`/`endproc` (with `uses`/`in`/`out`/`frame` contract checks), `struct`/`ends`,
  `comcall`, `sizeof`, `.include`. Data: BYTE WORD DWORD QWORD WCHAR, and real4/real8
  (`x real8 440.0` -> IEEE bits), with `N dup(v)`. Everything lowers to *visible*
  instructions -- nothing is hidden (see `--emit-asm`).

  Exceptions that bite (the full reference is help.md):
    * `invoke` uses rax/eax as scratch to stage stack args -- never pass an `invoke`
      argument that lives in rax/eax; route it through memory or another register.
    * xmm0-5 are volatile, xmm6-15 are callee-saved. The `uses` contract tracks GP
      registers ONLY, not xmm -- save xmm6+ yourself if a proc touches them.
    * a `proc` that contains `invoke`/`call` must declare `frame` (aligned shadow space).
    * float args to `invoke` need a real4/real8 annotation: `invoke f, real8 [rip+x]`.
    * `dup` count must be a literal: `BYTE 64 dup(0)`, not `BYTE 8*8 dup(0)`.
    * no manual `sub rsp` inside a `frame` proc -- use a memory slot or xmm0-5.
    * a `','` char literal trips the lexer -- use the ASCII number (44) instead.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut entry = "main".to_string();
    let mut emit_asm = false;
    let mut check = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                output = args.get(i + 1).cloned();
                i += 2;
            }
            "--entry" => {
                if let Some(e) = args.get(i + 1) {
                    entry = e.clone();
                }
                i += 2;
            }
            "--emit-asm" => {
                emit_asm = true;
                i += 1;
            }
            "--check" => {
                check = true;
                i += 1;
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => {
                input = Some(other.to_string());
                i += 1;
            }
        }
    }

    let Some(input) = input else {
        anyhow::bail!("no input file — try `was --help`");
    };

    let db = std::env::var("WINKB_DB")
        .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
    let kb = Kb::open(&db)?;

    let src = std::fs::read_to_string(&input)?;
    let src = was::expand_includes(&src, std::path::Path::new(&input))?;

    if check {
        let diags = was::check(&src, &kb);
        for d in &diags {
            if d.line == 0 {
                eprintln!("{input}: {}", d.message);
            } else {
                eprintln!("{input}:{}:{}: {}", d.line, d.col, d.message);
            }
        }
        eprintln!(
            "{input}: {}",
            if diags.is_empty() { "ok".to_string() } else { format!("{} issue(s)", diags.len()) }
        );
        return Ok(());
    }

    let lowered = was::lower(&src, &kb)?;

    if emit_asm {
        print!("{lowered}");
        return Ok(());
    }

    let module = assemble(&lowered)?;
    let output = output
        .unwrap_or_else(|| Path::new(&input).with_extension("obj").to_string_lossy().into_owned());

    if output.to_ascii_lowercase().ends_with(".exe") {
        // Self-contained PE: resolve each import's DLL via winkb, no linker.
        let mut map = BTreeMap::new();
        for ext in &module.externs {
            let dll = kb
                .function(ext)?
                .and_then(|f| f.dll)
                .ok_or_else(|| anyhow::anyhow!("no DLL known for import '{ext}'"))?;
            map.insert(ext.clone(), dll);
        }
        let exe = write_pe(&module, &map, &entry)?;
        std::fs::write(&output, &exe)?;
        eprintln!(
            "wrote {output}: {} bytes, entry '{entry}', imports {:?}",
            exe.len(),
            map,
        );
    } else {
        std::fs::write(&output, write_coff(&module))?;
        eprintln!(
            "wrote {output}: {} bytes .text, {} symbol(s), {} reloc(s), externs {:?}",
            module.code.len(),
            module.symbols.len(),
            module.relocs.len(),
            module.externs,
        );
    }
    Ok(())
}
