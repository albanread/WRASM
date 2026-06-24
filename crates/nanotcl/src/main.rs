//! nanotcl — the nano-TCL Tier-2 driver / introspector.
//!
//! Runs full TCL locally with `rust_tcl` and forwards game-proc calls as
//! `Verb reg=val, ...` lines down `\\.\pipe\nanotcl` to a running app. Two roles:
//!
//!   nanotcl script.tcl        run a TCL script (vars/expr/loops run LOCALLY,
//!                            only the substituted `Verb reg=val` lines cross)
//!   nanotcl                    REPL: one line per eval (pause / step / Disc rcx=…)
//!   nanotcl attach [N] [K]     INTROSPECT: frame-sync the game, sample every N
//!                            frames, single-step K samples, printing the register
//!                            state at each safe-state pause (the game blocks for
//!                            `cont` between samples). N default 1, K default 16.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use rust_tcl::{Arity, Registry, Value};

// ---------------------------------------------------------------------------
// raw Win32 named-pipe client (no extra crates)
// ---------------------------------------------------------------------------
type Handle = isize;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE: Handle = -1;

extern "system" {
    fn CreateFileA(name: *const u8, access: u32, share: u32, sec: *const c_void,
                   disp: u32, flags: u32, templ: Handle) -> Handle;
    fn WriteFile(h: Handle, buf: *const u8, n: u32, written: *mut u32, ovl: *const c_void) -> i32;
    fn ReadFile(h: Handle, buf: *mut u8, n: u32, read: *mut u32, ovl: *const c_void) -> i32;
    fn CloseHandle(h: Handle) -> i32;
}

struct Pipe(Handle);
// the handle is only ever touched under the Arc<Mutex<…>>
unsafe impl Send for Pipe {}

impl Pipe {
    fn open() -> Option<Pipe> {
        let name = b"\\\\.\\pipe\\nanotcl\0";
        let h = unsafe {
            CreateFileA(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, std::ptr::null(),
                        OPEN_EXISTING, 0, 0)
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
    fn recv(&self, buf: &mut [u8]) -> usize {
        let mut n = 0u32;
        let ok = unsafe {
            ReadFile(self.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null())
        };
        if ok == 0 { 0 } else { n as usize }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0); }
    }
}

// The verbs the gems app exposes: the library's standard verbs + the app's own +
// the introspection control verbs. (Sprint 3+: discover these from the game.)
const VERBS: &[&str] = &[
    "Pset", "Disc", "Circle", "Cls", "Line", "FillRect", "Text", "DrawSprite",
    "Snapshot", "Star", "step", "pause", "run", "intro", "free", "cont",
];

fn build_registry(pipe: Arc<Mutex<Pipe>>) -> Registry {
    let mut r = Registry::with_core();
    for &name in VERBS {
        let p = pipe.clone();
        r.register(name, Arity::at_least(0), move |_, args: &[Value]| {
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

// The 16 shadow registers a `frame …` ping carries, in ModRM order.
const REGNAMES: &[&str] = &[
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
    "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15",
];

// `nanotcl attach [every] [count]` — frame-sync the running game and single-step.
fn attach(pipe: &Arc<Mutex<Pipe>>, every: u32, count: u32) {
    let p = pipe.lock().unwrap();
    eprintln!("nanotcl: introspecting — sample every {every} frame(s), {count} samples");
    p.send(&format!("run\nintro rcx={every}"));
    let mut buf = [0u8; 1024];
    for i in 0..count {
        let n = p.recv(&mut buf);
        if n == 0 {
            eprintln!("nanotcl: pipe closed by the game");
            break;
        }
        let line = String::from_utf8_lossy(&buf[..n]);
        let line = line.trim();
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.first() == Some(&"frame") && parts.len() >= 17 {
            // show the non-zero registers (signed-decimal for the low 32 bits)
            let mut shown = Vec::new();
            for (k, name) in REGNAMES.iter().enumerate() {
                let v = u64::from_str_radix(parts[k + 1], 16).unwrap_or(0);
                if v != 0 {
                    shown.push(format!("{name}={}", v as i32));
                }
            }
            println!("sample {i:>3}:  {}", shown.join("  "));
        } else {
            println!("sample {i:>3}:  {line}");
        }
        p.send("cont");
    }
    p.send("free");
    eprintln!("nanotcl: released the game (free-running)");
}

fn main() {
    let pipe = match Pipe::open() {
        Some(p) => Arc::new(Mutex::new(p)),
        None => {
            eprintln!("nanotcl: cannot open \\\\.\\pipe\\nanotcl — is the game running (NANOTCL_LIVE)?");
            std::process::exit(1);
        }
    };

    let args: Vec<String> = std::env::args().collect();

    // attach / introspect mode
    if args.get(1).map(|s| s.as_str()) == Some("attach") {
        let every: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
        let count: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(16);
        attach(&pipe, every, count);
        return;
    }

    let r = build_registry(pipe);

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
        // REPL: each line is its own eval (wire verbs never re-fire)
        use std::io::{BufRead, Write};
        eprintln!("nanotcl REPL — verbs: pause run step Disc Circle Pset DrawSprite Star Snapshot intro …  (Ctrl-Z to exit)");
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
