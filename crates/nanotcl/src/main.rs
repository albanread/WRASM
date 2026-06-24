//! nanotcl — the nano-TCL Tier-2 driver / introspector.
//!
//! Runs full TCL locally with `rust_tcl` and forwards game-proc calls as
//! `Verb reg=val, ...` lines down `\\.\pipe\nanotcl` to a running app.
//!
//!   nanotcl script.tcl              run a TCL script (loops/vars run LOCALLY,
//!                                  only the substituted `Verb reg=val` lines cross)
//!   nanotcl                          REPL: one line per eval (pause / step / Disc …)
//!   nanotcl attach [N] [K]           INTROSPECT: frame-sync the game, sample every N
//!                                  frames, single-step K samples, printing the
//!                                  register state at each safe-state pause.
//!   nanotcl attach N K hook.tcl      AGENT TEST HARNESS: run `hook.tcl` at each
//!                                  sample with the registers exposed as TCL vars
//!                                  (rax, rcx, … and prev_rax, …). The hook can
//!                                  `assert`, forward draw/call verbs, and `puts`.
//!                                  Exit code is non-zero if any assert failed —
//!                                  so an agent can drive + check a game in CI.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
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
unsafe impl Send for Pipe {} // only touched under the Arc<Mutex<…>>

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
        let mut w = 0u32;
        unsafe { WriteFile(self.0, s.as_ptr(), s.len() as u32, &mut w, std::ptr::null()); }
    }
    fn recv(&self, buf: &mut [u8]) -> usize {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(self.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null()) };
        if ok == 0 { 0 } else { n as usize }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) { unsafe { CloseHandle(self.0); } }
}

// game verbs (library standard + the app's + introspection control), forwarded as-is.
const VERBS: &[&str] = &[
    "Pset", "Disc", "Circle", "Cls", "Line", "FillRect", "Text", "DrawSprite",
    "Snapshot", "Star", "step", "pause", "run", "intro", "free", "cont",
];

// the 16 shadow registers a `frame …` ping carries, in ModRM order.
const REGNAMES: &[&str] = &[
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
    "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15",
];

fn build_registry(pipe: Arc<Mutex<Pipe>>, failed: Arc<AtomicBool>) -> Registry {
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
    // `assert <cond> [msg]` — agent-side check; falsy cond => print + set failure flag
    let f = failed.clone();
    r.register("assert", Arity::at_least(1), move |_, args: &[Value]| {
        let c = args[0].as_str();
        let truthy = !(c.is_empty() || c == "0" || c == "false");
        if !truthy {
            let msg = args.get(1).map(|a| a.as_str()).unwrap_or("assertion failed");
            eprintln!("ASSERT FAILED: {msg}");
            f.store(true, Ordering::SeqCst);
        }
        Ok(Value::new(if truthy { "1" } else { "0" }))
    });
    // `send <line…>` — forward a raw verb line to the game. Generic: drives ANY
    // app's verbs (setpiece, drop, …) with no hardcoding in this tool.
    let p = pipe.clone();
    r.register("send", Arity::at_least(1), move |_, args: &[Value]| {
        let line = args.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(" ");
        p.lock().unwrap().send(&line);
        Ok(Value::new(""))
    });
    // `regs` — take ONE frame-sync snapshot and return the 16 shadow registers as
    // a TCL list (decimal). Lets a script read game state a game published via
    // TclReg: `set s [regs]; lindex $s 0`. Order: rax rcx rdx rbx rsp rbp rsi rdi r8..r15.
    let p = pipe.clone();
    r.register("regs", Arity::at_least(0), move |_, _args: &[Value]| {
        let pp = p.lock().unwrap();
        pp.send("intro rcx=1");
        let mut buf = [0u8; 1024];
        let n = pp.recv(&mut buf);
        let line = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        pp.send("free");
        drop(pp);
        let regs = parse_ping(&line).unwrap_or([0i32; 16]);
        let s = regs.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");
        Ok(Value::new(s))
    });
    r
}

// parse a `frame HEX HEX …` ping into 16 signed-32-bit register values.
fn parse_ping(line: &str) -> Option<[i32; 16]> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.first() != Some(&"frame") || parts.len() < 17 {
        return None;
    }
    let mut regs = [0i32; 16];
    for i in 0..16 {
        regs[i] = u64::from_str_radix(parts[i + 1], 16).ok()? as u32 as i32;
    }
    Some(regs)
}

// `nanotcl attach [every] [count] [hook.tcl]` — frame-sync + single-step the game.
fn attach(reg: &Registry, pipe: &Arc<Mutex<Pipe>>, every: u32, count: u32, hook: Option<&str>) {
    let hook_src = hook.map(|path| {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("nanotcl: {path}: {e}");
            std::process::exit(2);
        })
    });
    eprintln!("nanotcl: introspecting — every {every} frame(s), {count} sample(s){}",
        if hook.is_some() { format!(", hook {}", hook.unwrap()) } else { String::new() });
    pipe.lock().unwrap().send(&format!("run\nintro rcx={every}"));

    let mut prev = [0i32; 16];
    let mut buf = [0u8; 1024];
    for i in 0..count {
        let n = pipe.lock().unwrap().recv(&mut buf);   // lock only for the blocking read
        if n == 0 { eprintln!("nanotcl: pipe closed by the game"); break; }
        let line = String::from_utf8_lossy(&buf[..n]);
        let line = line.trim();
        let regs = match parse_ping(line) {
            Some(r) => r,
            None => { println!("sample {i:>3}:  {line}"); pipe.lock().unwrap().send("cont"); continue; }
        };
        match &hook_src {
            Some(src) => {
                // expose this frame's + the previous frame's registers as TCL vars
                let mut pre = String::new();
                for (k, name) in REGNAMES.iter().enumerate() {
                    pre.push_str(&format!("set {name} {}\nset prev_{name} {}\n", regs[k], prev[k]));
                }
                pre.push_str(&format!("set sample {i}\n"));
                // NOTE: lock is released here, so the hook's forward-verbs can re-lock
                match rust_tcl::eval(&(pre + src), reg) {
                    Ok(res) => print!("{}", res.output),
                    Err(e) => eprintln!("sample {i}: tcl error: {}", e.message),
                }
            }
            None => {
                let shown: Vec<String> = REGNAMES.iter().enumerate()
                    .filter(|(k, _)| regs[*k] != 0)
                    .map(|(k, n)| format!("{n}={}", regs[k]))
                    .collect();
                println!("sample {i:>3}:  {}", shown.join("  "));
            }
        }
        prev = regs;
        pipe.lock().unwrap().send("cont");
    }
    pipe.lock().unwrap().send("free");
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
    let failed = Arc::new(AtomicBool::new(false));
    let r = build_registry(pipe.clone(), failed.clone());
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(|s| s.as_str()) == Some("attach") {
        let every: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
        let count: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(16);
        let hook = args.get(4).map(|s| s.as_str());
        attach(&r, &pipe, every, count, hook);
        std::process::exit(if failed.load(Ordering::SeqCst) { 1 } else { 0 });
    }

    if args.len() > 1 {
        // script mode: a single eval — full TCL runs locally
        let path = &args[1];
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("nanotcl: {path}: {e}"); std::process::exit(2);
        });
        match rust_tcl::eval(&src, &r) {
            Ok(res) => print!("{}", res.output),
            Err(e) => { eprintln!("nanotcl: tcl error: {}", e.message); std::process::exit(2); }
        }
        std::process::exit(if failed.load(Ordering::SeqCst) { 1 } else { 0 });
    }

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
