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
                eprintln!(
                    "usage: was <input.asm> [-o <out.obj|out.exe>] [--entry NAME] [--emit-asm]\n\
                     (a .exe output is a self-contained PE; otherwise a COFF object)"
                );
                return Ok(());
            }
            other => {
                input = Some(other.to_string());
                i += 1;
            }
        }
    }

    let Some(input) = input else {
        anyhow::bail!("no input file\nusage: was <input.asm> [-o <output.obj>] [--emit-asm]");
    };

    let db = std::env::var("WINKB_DB")
        .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
    let kb = Kb::open(&db)?;

    let src = std::fs::read_to_string(&input)?;

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
