//! was — assemble a Windows source file to a COFF object, resolving `invoke`,
//! Windows constants, struct fields, and `sizeof` through winkb.
//!
//!   was input.asm -o output.obj
//!   was input.asm --emit-asm        # print the lowered rasm text and stop
//!
//! Knowledge DB: $WINKB_DB, else E:\windows_api\windows_api.db.

use std::path::Path;
use std::process::ExitCode;

use rasm::{assemble, write_coff};
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
    let mut emit_asm = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                output = args.get(i + 1).cloned();
                i += 2;
            }
            "--emit-asm" => {
                emit_asm = true;
                i += 1;
            }
            "-h" | "--help" => {
                eprintln!("usage: was <input.asm> [-o <output.obj>] [--emit-asm]");
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
    let lowered = was::lower(&src, &kb)?;

    if emit_asm {
        print!("{lowered}");
        return Ok(());
    }

    let module = assemble(&lowered)?;
    let output = output
        .unwrap_or_else(|| Path::new(&input).with_extension("obj").to_string_lossy().into_owned());
    std::fs::write(&output, write_coff(&module))?;
    eprintln!(
        "wrote {output}: {} bytes .text, {} symbol(s), {} reloc(s), externs {:?}",
        module.code.len(),
        module.symbols.len(),
        module.relocs.len(),
        module.externs,
    );
    Ok(())
}
