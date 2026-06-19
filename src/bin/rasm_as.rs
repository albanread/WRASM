//! rasm-as — assemble Intel-syntax text into a COFF object using rasm.
//!
//!   rasm-as input.asm -o output.obj
//!
//! Link the result into an executable, e.g.:
//!   lld-link output.obj kernel32.lib /entry:main /subsystem:console

use std::path::Path;
use std::process::ExitCode;

use rasm::{assemble, write_coff};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                output = args.get(i + 1).cloned();
                i += 2;
            }
            "-h" | "--help" => {
                eprintln!("usage: rasm-as <input.asm> [-o <output.obj>]");
                return ExitCode::SUCCESS;
            }
            other => {
                input = Some(other.to_string());
                i += 1;
            }
        }
    }

    let Some(input) = input else {
        eprintln!("error: no input file\nusage: rasm-as <input.asm> [-o <output.obj>]");
        return ExitCode::FAILURE;
    };

    let text = match std::fs::read_to_string(&input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: reading {input}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let module = match assemble(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: assembling {input}: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let output = output.unwrap_or_else(|| {
        Path::new(&input).with_extension("obj").to_string_lossy().into_owned()
    });

    let obj = write_coff(&module);
    if let Err(e) = std::fs::write(&output, &obj) {
        eprintln!("error: writing {output}: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "wrote {output}: {} bytes .text, {} symbol(s), {} reloc(s), {} extern(s)",
        module.code.len(),
        module.symbols.len(),
        module.relocs.len(),
        module.externs.len(),
    );
    ExitCode::SUCCESS
}
