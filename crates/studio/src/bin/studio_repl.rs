//! studio-repl — a terminal REPL that drives the language thread.
//!
//! The whole IDE backbone, hands-on, before any window exists: each line you
//! type is posted to the language thread (which owns winkb + was + rasm) and the
//! reply is printed. This is the Corman Lisp shape — a prompt in front of a
//! language image running on its own thread.
//!
//!   <anything>          a card for that query (function / struct / search)
//!   :frame <func>       the interactive insert line ({{field}}/{{select}})
//!   :suggest <name>     did-you-mean
//!   :check  <file>      semantic diagnostics for a source file
//!   :asm    <file>      print the lowered rasm text
//!   :obj    <file> [out]  assemble to a COFF .obj
//!   :exe    <file> [out]  assemble to a self-contained .exe
//!   :q                  quit
//!
//! DB path: $WINKB_DB, else E:\windows_api\windows_api.db.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use studio::lang::{Emit, Lang, Response};

fn main() -> ExitCode {
    let db = std::env::var("WINKB_DB")
        .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
    let lang = match Lang::spawn(&db) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("studio-repl — language thread up (db: {db}). Type :q to quit, a name for a card.");

    let stdin = io::stdin();
    loop {
        print!("was> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == ":q" || line == ":quit" {
            break;
        }
        handle(&lang, line);
    }
    lang.shutdown();
    ExitCode::SUCCESS
}

fn handle(lang: &Lang, line: &str) {
    let (cmd, rest) = match line.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (line, ""),
    };
    match cmd {
        ":frame" => match lang.frame(rest) {
            Some(Response::Frame { line: Some(s), .. }) => println!("{s}"),
            Some(Response::Frame { line: None, .. }) => eprintln!("function '{rest}' not found"),
            other => report(other),
        },
        ":suggest" => match lang.suggest(rest) {
            Some(Response::Suggest { names, .. }) if names.is_empty() => eprintln!("(no suggestions)"),
            Some(Response::Suggest { names, .. }) => println!("{}", names.join("\n")),
            other => report(other),
        },
        ":check" => match read_file(rest) {
            Ok(src) => match lang.check_src(&src) {
                Some(Response::Check { diags, .. }) if diags.is_empty() => println!("ok — no issues"),
                Some(Response::Check { diags, .. }) => {
                    for d in diags {
                        if d.line == 0 {
                            println!("{}: {}", rest, d.message);
                        } else {
                            println!("{}:{}:{}: {}", rest, d.line, d.col, d.message);
                        }
                    }
                }
                other => report(other),
            },
            Err(e) => eprintln!("error: {e}"),
        },
        ":asm" => assemble_cmd(lang, rest, Emit::Asm),
        ":obj" => assemble_cmd(lang, rest, Emit::Obj),
        ":exe" => assemble_cmd(lang, rest, Emit::Exe),
        _ => match lang.card(line) {
            Some(Response::Card { markdown, .. }) => print!("{markdown}"),
            other => report(other),
        },
    }
}

fn assemble_cmd(lang: &Lang, rest: &str, emit: Emit) {
    let mut parts = rest.splitn(2, char::is_whitespace);
    let Some(file) = parts.next().filter(|s| !s.is_empty()) else {
        eprintln!("usage: :{} <file> [out]", emit_name(emit));
        return;
    };
    let out = parts.next().map(str::trim).filter(|s| !s.is_empty());
    let src = match read_file(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };
    match lang.assemble(&src, emit) {
        Some(Response::Assembled { bytes, info, .. }) => {
            if emit == Emit::Asm {
                io::stdout().write_all(&bytes).ok();
            } else {
                let out = out
                    .map(str::to_string)
                    .unwrap_or_else(|| default_out(file, emit));
                match std::fs::write(&out, &bytes) {
                    Ok(()) => println!("wrote {out} — {info}"),
                    Err(e) => eprintln!("error writing {out}: {e}"),
                }
            }
        }
        other => report(other),
    }
}

fn report(resp: Option<Response>) {
    match resp {
        Some(Response::Error { message, .. }) => eprintln!("error: {message}"),
        None => eprintln!("error: language thread timed out"),
        Some(other) => eprintln!("unexpected reply: {other:?}"),
    }
}

fn read_file(path: &str) -> io::Result<String> {
    if path.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file given"));
    }
    std::fs::read_to_string(path)
}

fn emit_name(emit: Emit) -> &'static str {
    match emit {
        Emit::Asm => "asm",
        Emit::Obj => "obj",
        Emit::Exe => "exe",
    }
}

fn default_out(file: &str, emit: Emit) -> String {
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    format!("{stem}.{}", emit_name(emit))
}
