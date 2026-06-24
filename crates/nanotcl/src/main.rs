//! nanotcl — the nano-TCL Tier-2 driver.
//!
//! Runs full TCL locally with `rust_tcl` (`set`/`expr`/`if`/`while`/`foreach`/
//! `proc`, `$vars`, `[cmd subst]`) and forwards game-proc calls as
//! `Verb reg=val, ...` lines down `\\.\pipe\nanotcl` to a running app. The smart
//! tier lives here; the game stays the dumb-but-real register-machine executor.
//!
//!   nanotcl script.tcl     run a TCL script (vars/expr/loops execute LOCALLY,
//!                          only the substituted `Verb reg=val` lines cross the wire)
//!   nanotcl                REPL: one line per eval — type `pause`, `step`, `Disc rcx=…`

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use rust_tcl::{Arity, Registry, Value};

// ---------------------------------------------------------------------------
// raw Win32 named-pipe client (no extra crates)
// ---------------------------------------------------------------------------
type Handle = isize;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE: Handle = -1;

extern "system" {
    fn CreateFileA(name: *const u8, access: u32, share: u32, sec: *const c_void,
                   disp: u32, flags: u32, templ: Handle) -> Handle;
    fn WriteFile(h: Handle, buf: *const u8, n: u32, written: *mut u32, ovl: *const c_void) -> i32;
    fn CloseHandle(h: Handle) -> i32;
}

struct Pipe(Handle);
// the handle is only ever touched under the Arc<Mutex<…>>
unsafe impl Send for Pipe {}

impl Pipe {
    fn open() -> Option<Pipe> {
        let name = b"\\\\.\\pipe\\nanotcl\0";
        let h = unsafe {
            CreateFileA(name.as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, 0)
        };
        if h == INVALID_HANDLE { None } else { Some(Pipe(h)) }
    }
    fn send(&self, line: &str) {
        let mut s = String::with_capacity(line.len() + 1);
        s.push_str(line);
        s.push('\n');
        let mut written = 0u32;
        unsafe {
            WriteFile(self.0, s.as_ptr(), s.len() as u32, &mut written, std::ptr::null());
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0); }
    }
}

// The verbs the gems app exposes: the library's standard verbs + the app's own.
// (Sprint 3 will discover these from the game instead of hard-coding them.)
const VERBS: &[&str] = &[
    "Pset", "Disc", "Circle", "Cls", "Line", "FillRect", "Text", "DrawSprite",
    "Snapshot", "Star", "step", "pause", "run",
];

fn build_registry(pipe: Arc<Mutex<Pipe>>) -> Registry {
    let mut r = Registry::with_core();
    for &name in VERBS {
        let p = pipe.clone();
        r.register(name, Arity::at_least(0), move |_, args: &[Value]| {
            // args are already $var/[expr]-substituted by rust_tcl; just forward.
            let line = if args.is_empty() {
                name.to_string()
            } else {
                let joined = args.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ");
                format!("{} {}", name, joined)
            };
            p.lock().unwrap().send(&line);
            Ok(Value::new(""))
        });
    }
    r
}

fn main() {
    let pipe = match Pipe::open() {
        Some(p) => Arc::new(Mutex::new(p)),
        None => {
            eprintln!("nanotcl: cannot open \\\\.\\pipe\\nanotcl — is the game running (NANOTCL_LIVE)?");
            std::process::exit(1);
        }
    };
    let r = build_registry(pipe);

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        // script mode: a single eval — full TCL (vars/expr/loops run locally)
        let path = &args[1];
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("nanotcl: {path}: {e}");
            std::process::exit(1);
        });
        match rust_tcl::eval(&src, &r) {
            Ok(res) => print!("{}", res.output),
            Err(e) => {
                eprintln!("nanotcl: tcl error: {}", e.message);
                std::process::exit(1);
            }
        }
    } else {
        // REPL: each line is its own eval (no cross-line vars yet, but wire verbs
        // never re-fire — a persistent-Vm REPL is a later refinement).
        use std::io::{BufRead, Write};
        eprintln!("nanotcl REPL — verbs: pause run step Disc Circle Pset DrawSprite Star Snapshot …  (Ctrl-Z to exit)");
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = match line { Ok(l) => l, Err(_) => break };
            if line.trim().is_empty() { continue; }
            match rust_tcl::eval(&line, &r) {
                Ok(res) => { print!("{}", res.output); std::io::stdout().flush().ok(); }
                Err(e) => eprintln!("err: {}", e.message),
            }
        }
    }
}
