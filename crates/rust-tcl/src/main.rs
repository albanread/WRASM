use std::io::Read;
use std::process;

use rust_tcl::{Registry, Vm, cfg, codegen, lexer, parser, sema};

const USAGE: &str = "\
rust-tcl - bytecode Tool Command Language core

USAGE:
  rust-tcl run [--dump-cfg] [--dump-bytecode] [--result] [-e SCRIPT] [FILE]
  rust-tcl --help

If FILE is omitted, the script is read from stdin.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match dispatch(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rust-tcl: {err}");
            2
        }
    };
    process::exit(code);
}

fn dispatch(args: &[String]) -> Result<i32, String> {
    match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("run") => run(&args[1..]),
        Some(other) => Err(format!("unknown command `{other}`")),
    }
}

fn run(args: &[String]) -> Result<i32, String> {
    let mut dump_cfg = false;
    let mut dump_bytecode = false;
    let mut print_result = false;
    let mut inline: Option<String> = None;
    let mut file: Option<String> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dump-cfg" => dump_cfg = true,
            "--dump-bytecode" => dump_bytecode = true,
            "--result" => print_result = true,
            "-e" => inline = Some(it.next().ok_or("`-e` needs a script argument")?.to_string()),
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(0);
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option `{other}`"));
            }
            other => file = Some(other.to_string()),
        }
    }

    let source = match inline {
        Some(source) => source,
        None => read_input(file.as_deref())?,
    };

    let registry = Registry::with_core();
    let tokens = lexer::lex(&source).map_err(|e| e.to_string())?;
    let ast = parser::parse(&tokens).map_err(|e| e.to_string())?;
    let bound = sema::analyze(&ast, &registry).map_err(|e| e.to_string())?;
    let graph = cfg::build(&bound);
    if dump_cfg {
        eprintln!("{graph:#?}");
    }
    let program = codegen::compile(&graph, &registry).map_err(|e| e.to_string())?;
    if dump_bytecode {
        eprintln!("{program:#?}");
    }
    let result = Vm::new(&registry)
        .run(&program)
        .map_err(|e| e.to_string())?;
    print!("{}", result.output);
    if print_result {
        println!("{}", result.value);
    }
    Ok(0)
}

fn read_input(file: Option<&str>) -> Result<String, String> {
    match file {
        None | Some("-") => {
            let mut source = String::new();
            std::io::stdin()
                .read_to_string(&mut source)
                .map_err(|e| format!("reading stdin: {e}"))?;
            Ok(source)
        }
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("reading `{path}`: {e}")),
    }
}
