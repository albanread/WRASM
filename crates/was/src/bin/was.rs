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
  was <input.was> --include-graph        print the .include dependency tree, then stop
  was <input.was> -o x.exe --entry NAME  set the PE entry symbol (default: main)
  was <input.was> --nomodules            disable module-scoped labels (ON by default)
  was -h | --help                        this help

The knowledge DB ($WINKB_DB, else E:\\windows_api\\windows_api.db) resolves `invoke`
signatures, Windows constants, struct fields, and `sizeof`.

THE WRASM DIALECT — Intel/MASM-compatible, with exceptions
  Intel syntax (`mov dst, src`; `[rip + label]`) plus MASM-style macros: `invoke`,
  `proc`/`endproc` (with `uses`/`in`/`out`/`frame` contract checks), `struct`/`ends`,
  `comcall`, `sizeof`, `.include`. Data: BYTE WORD DWORD QWORD WCHAR, and real4/real8
  (`x real8 440.0` -> IEEE bits), with `N dup(v)`. Everything lowers to *visible*
  instructions -- nothing is hidden (see `--emit-asm`).

EQUATES + COMPILE-TIME CONDITIONALS (MASM-style, fold before lowering)
  `NAME equ <expr>` / `NAME = <expr>` define an integer constant (define-before-use);
  every later whole-word use of NAME folds to its value (never inside a \"string\" or a
  comment). `<expr>` is a full integer expression: decimal/0x-hex literals, equates,
  and winkb constants, with - ~  * / %  + -  << >>  &  ^  |  and parentheses.
  Inside a `struct`/`ends` data block, `field = value` stays a struct field, NOT an
  equate. Undotted COMPILE-TIME conditionals select source text before assembly:
    IF <expr> / IFDEF NAME / IFNDEF NAME / ELSEIF <expr> / ELSE / ENDIF  (nestable)
  These are DISTINCT from the runtime `.if` (which lowers to a compare + branch).

MODULES (ON by default; --nomodules to disable; a narrowing of \"every label is global\")
  A module is `module NAME … endmodule` markers in a file; the region scopes only
  THAT file's own lines -- a file pulled in by `.include` is NOT absorbed (it keeps
  its own module, or none). A module may span many files (each declares `module
  NAME`), and a file may hold several module regions. A label whose name starts with
  a CAPITAL is EXPORTED (global, unique, callable from any module); a lowercase/_
  name is PRIVATE -- mangled `NAME$label`, so two modules may reuse a helper name
  (`loop`, `pu_loop`) without colliding. A label inside a `proc … endproc` is finer
  still -- private to that PROC (`proc$label`), so two procs in one module may reuse a
  jump-target name (`loop`, `done`). `.globl` also pins a name global. With
  --nomodules, `module`/`endmodule` are ignored and every label stays global
  (byte-identical to the pre-modules behaviour). `--include-graph` shows the structure.

  Exceptions that bite (the full reference is help.md):
    * `invoke` uses rax/eax as scratch to stage stack args -- never pass an `invoke`
      argument that lives in rax/eax; route it through memory or another register.
    * xmm0-5 are volatile, xmm6-15 are callee-saved. The `uses` contract tracks GP
      registers ONLY, not xmm -- save xmm6+ yourself if a proc touches them.
    * a `proc` that contains `invoke`/`call` must declare `frame` (aligned shadow space).
    * float args to `invoke` need a real4/real8 annotation: `invoke f, real8 [rip+x]`.
    * `dup` count folds via equates/constants: `STRIDE equ 8*8` then `BYTE STRIDE dup(0)`
      (a bare `8*8` still won't parse -- name it with an equate first).
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
    let mut modules = true; // module-scoped labels are ON by default; --nomodules opts out
    let mut include_graph = false;
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
            "--modules" => {
                modules = true; // explicit; this is the default
                i += 1;
            }
            "--nomodules" => {
                modules = false;
                i += 1;
            }
            "--include-graph" => {
                include_graph = true;
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

    let raw = std::fs::read_to_string(&input)?;
    let exp = was::expand_includes_graph(&raw, std::path::Path::new(&input))?;

    if include_graph {
        print_include_graph(&exp);
        return Ok(());
    }

    // The module overlay (opt-in) needs the per-line file attribution, so it runs
    // here on the expanded text; otherwise we take the expanded text as-is.
    let src = if modules { was::scope_modules_by_file(&exp) } else { exp.text };

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

    // Keep the source→lowered map so a downstream encode error is reported at the
    // real source line, not the post-preprocessing/lowering line (the equate/IF
    // pass removes lines, so the two diverge).
    let (lowered, map) = was::lower_mapped(&src, &kb)?;

    if emit_asm {
        print!("{lowered}");
        return Ok(());
    }

    let module = assemble(&lowered).map_err(|e| was::remap_assemble_error(e, &map))?;
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

/// Print the `.include` dependency tree (DFS, in include order) with per-file line
/// counts and the parent line each include sits on — the flattened graph `was` now
/// records, so the shell-of-includes structure is visible at a glance.
fn print_include_graph(g: &was::Expansion) {
    let mut lines = vec![0usize; g.files.len()];
    for &f in &g.line_file {
        lines[f as usize] += 1;
    }
    fn walk(g: &was::Expansion, lines: &[usize], file: u32, depth: usize, via: Option<u32>) {
        let indent = "  ".repeat(depth);
        let name = g.files[file as usize].display();
        match via {
            Some(ln) => {
                println!("{indent}{name}  [{} lines, .include at parent:{ln}]", lines[file as usize])
            }
            None => println!("{indent}{name}  [{} lines]", lines[file as usize]),
        }
        for &(p, c, ln) in &g.edges {
            if p == file {
                walk(g, lines, c, depth + 1, Some(ln));
            }
        }
    }
    walk(g, &lines, 0, 0, None);
    let total: usize = lines.iter().sum();
    println!("\n{} files, {} expanded lines", g.files.len(), total);
}
