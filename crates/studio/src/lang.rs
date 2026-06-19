//! lang.rs — the language thread.
//!
//! All of the IDE's knowledge-and-assembler state — winkb's `Kb` (which wraps a
//! `!Sync` SQLite connection), plus the `was` front-end and the `rasm` encoder —
//! lives on ONE worker thread. The GUI never touches it directly: it posts
//! [`Request`]s and drains [`Response`]s over channels.
//!
//! This is the Corman Lisp / WF66 "language thread" pattern. In Corman Lisp the
//! Lisp image runs on its own thread and the editor sends it forms to evaluate,
//! reading results back; WF66 does the same for its Forth session (see
//! `igui/channels.rs`). The payoff is identical here: the Direct2D message loop
//! never blocks on a db query or an assemble, and the one place that owns the
//! non-`Sync` connection is unambiguous.
//!
//! Because it is pure message-passing, the entire backbone is headless-testable
//! — spawn, post, assert the reply — with no window in sight. Two ways to drive
//! it:
//!   * GUI style (async): [`Lang::post_card`] etc. return a request id; the loop
//!     calls [`Lang::poll`] each pass and matches replies by [`Response::id`].
//!   * Sync style (tests / a REPL): [`Lang::card`] etc. post and block for the
//!     matching reply, with the WF66-standard 5-second guard.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use winkb::Kb;

/// What an assemble should emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emit {
    /// The lowered rasm text (what `was --emit-asm` prints).
    Asm,
    /// A COFF `.obj`.
    Obj,
    /// A self-contained PE `.exe` (imports resolved via winkb).
    Exe,
}

/// A diagnostic, decoupled from `was::Diag` so the GUI/tests need not depend on
/// `was`. `line`/`col` are 1-based; `line == 0` marks a whole-file message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

/// A unit of work for the language thread. `id` correlates the reply.
#[derive(Debug, Clone)]
pub enum Request {
    /// Markdown card for a free-form query (function/struct/interface/search).
    Card { id: u64, query: String },
    /// The interactive insert line for a function (`{{field}}`/`{{select}}`).
    Frame { id: u64, func: String },
    /// Semantic check of a source buffer.
    Check { id: u64, src: String },
    /// Assemble a source buffer to asm text / object / exe bytes.
    Assemble { id: u64, src: String, emit: Emit },
    /// Did-you-mean suggestions for a name.
    Suggest { id: u64, name: String },
    /// Stop the worker and let the join complete.
    Shutdown,
}

/// A reply from the language thread, tagged with the originating request `id`.
#[derive(Debug, Clone)]
pub enum Response {
    Card { id: u64, markdown: String },
    Frame { id: u64, line: Option<String> },
    Check { id: u64, diags: Vec<Diag> },
    Assembled { id: u64, emit: Emit, bytes: Vec<u8>, info: String },
    Suggest { id: u64, names: Vec<String> },
    /// A request failed; `id` is the originating request's id.
    Error { id: u64, message: String },
}

impl Response {
    /// The id of the request this reply answers.
    pub fn id(&self) -> u64 {
        match self {
            Response::Card { id, .. }
            | Response::Frame { id, .. }
            | Response::Check { id, .. }
            | Response::Assembled { id, .. }
            | Response::Suggest { id, .. }
            | Response::Error { id, .. } => *id,
        }
    }
}

/// A handle to the language thread. Send + drainable; lives on the GUI thread.
pub struct Lang {
    tx: Sender<Request>,
    rx: Receiver<Response>,
    join: Option<JoinHandle<()>>,
    next_id: AtomicU64,
}

impl Lang {
    /// Spawn the worker, opening the knowledge db on it. Blocks until the worker
    /// confirms the db opened (so a bad path surfaces here, not asynchronously).
    pub fn spawn(db: &str) -> Result<Lang> {
        let (tx_req, rx_req) = channel::<Request>();
        let (tx_resp, rx_resp) = channel::<Response>();
        let (tx_ready, rx_ready) = channel::<Result<(), String>>();
        let db = db.to_string();
        let join = std::thread::Builder::new()
            .name("studio-language".into())
            .spawn(move || worker(db, tx_ready, rx_req, tx_resp))?;
        match rx_ready.recv() {
            Ok(Ok(())) => Ok(Lang {
                tx: tx_req,
                rx: rx_resp,
                join: Some(join),
                next_id: AtomicU64::new(1),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                anyhow::bail!("language thread: open db: {e}")
            }
            Err(_) => {
                let _ = join.join();
                anyhow::bail!("language thread died during startup")
            }
        }
    }

    /// Allocate the next request id.
    pub fn alloc(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // ── GUI style: fire and drain ───────────────────────────────────────────

    pub fn post_card(&self, query: &str) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Card { id, query: query.to_string() });
        id
    }
    pub fn post_frame(&self, func: &str) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Frame { id, func: func.to_string() });
        id
    }
    pub fn post_check(&self, src: &str) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Check { id, src: src.to_string() });
        id
    }
    pub fn post_assemble(&self, src: &str, emit: Emit) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Assemble { id, src: src.to_string(), emit });
        id
    }
    pub fn post_suggest(&self, name: &str) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Suggest { id, name: name.to_string() });
        id
    }

    /// Non-blocking drain — the GUI calls this each message-loop pass.
    pub fn poll(&self) -> Option<Response> {
        self.rx.try_recv().ok()
    }

    /// Block for the next reply up to `ms` milliseconds.
    pub fn recv_timeout(&self, ms: u64) -> Option<Response> {
        self.rx.recv_timeout(Duration::from_millis(ms)).ok()
    }

    // ── Sync style: post and wait for the matching reply ─────────────────────

    /// Post a request and block for *its* reply (matching id), with the
    /// WF66-standard 5-second guard. Returns `None` on timeout or a dead worker.
    pub fn call(&self, make: impl FnOnce(u64) -> Request) -> Option<Response> {
        let id = self.alloc();
        self.tx.send(make(id)).ok()?;
        loop {
            match self.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(r) if r.id() == id => return Some(r),
                Ok(_) => continue, // a stale async reply; keep waiting for ours
                Err(_) => return None,
            }
        }
    }

    pub fn card(&self, query: &str) -> Option<Response> {
        self.call(|id| Request::Card { id, query: query.to_string() })
    }
    pub fn frame(&self, func: &str) -> Option<Response> {
        self.call(|id| Request::Frame { id, func: func.to_string() })
    }
    pub fn check_src(&self, src: &str) -> Option<Response> {
        self.call(|id| Request::Check { id, src: src.to_string() })
    }
    pub fn assemble(&self, src: &str, emit: Emit) -> Option<Response> {
        self.call(|id| Request::Assemble { id, src: src.to_string(), emit })
    }
    pub fn suggest(&self, name: &str) -> Option<Response> {
        self.call(|id| Request::Suggest { id, name: name.to_string() })
    }

    /// Stop the worker and wait for it to finish.
    pub fn shutdown(mut self) {
        let _ = self.tx.send(Request::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Lang {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The worker body: own the `Kb`, drain requests, never let an error kill the
/// thread (every failure becomes a `Response::Error`).
fn worker(
    db: String,
    ready: Sender<Result<(), String>>,
    rx: Receiver<Request>,
    tx: Sender<Response>,
) {
    let kb = match Kb::open(&db) {
        Ok(k) => {
            let _ = ready.send(Ok(()));
            k
        }
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return;
        }
    };

    while let Ok(req) = rx.recv() {
        let resp = match req {
            Request::Shutdown => break,
            Request::Card { id, query } => match ide::answer(&kb, &query) {
                Ok(markdown) => Response::Card { id, markdown },
                Err(e) => Response::Error { id, message: format!("{e:#}") },
            },
            Request::Frame { id, func } => match ide::insert_frame(&kb, &func) {
                Ok(line) => Response::Frame { id, line },
                Err(e) => Response::Error { id, message: format!("{e:#}") },
            },
            Request::Check { id, src } => {
                let diags = was::check(&src, &kb)
                    .into_iter()
                    .map(|d| Diag { line: d.line, col: d.col, message: d.message })
                    .collect();
                Response::Check { id, diags }
            }
            Request::Assemble { id, src, emit } => match assemble_bytes(&kb, &src, emit) {
                Ok((bytes, info)) => Response::Assembled { id, emit, bytes, info },
                Err(e) => Response::Error { id, message: format!("{e:#}") },
            },
            Request::Suggest { id, name } => match kb.suggest(&name, 5) {
                Ok(names) => Response::Suggest { id, names },
                Err(e) => Response::Error { id, message: format!("{e:#}") },
            },
        };
        if tx.send(resp).is_err() {
            break; // the GUI handle was dropped; nothing left to answer
        }
    }
}

/// Lower + assemble a source buffer to the requested artifact, returning the
/// bytes and a one-line status string for the GUI.
fn assemble_bytes(kb: &Kb, src: &str, emit: Emit) -> Result<(Vec<u8>, String)> {
    let lowered = was::lower(src, kb)?;
    if emit == Emit::Asm {
        return Ok((lowered.into_bytes(), "asm".to_string()));
    }
    let module = rasm::assemble(&lowered)?;
    match emit {
        Emit::Asm => unreachable!("handled above"),
        Emit::Obj => {
            let bytes = rasm::write_coff(&module);
            let info = format!(
                "obj: {} bytes .text, {} sym, {} reloc, externs {:?}",
                module.code.len(),
                module.symbols.len(),
                module.relocs.len(),
                module.externs,
            );
            Ok((bytes, info))
        }
        Emit::Exe => {
            let mut map = BTreeMap::new();
            for ext in &module.externs {
                let dll = kb
                    .function(ext)?
                    .and_then(|f| f.dll)
                    .ok_or_else(|| anyhow::anyhow!("no DLL known for import '{ext}'"))?;
                map.insert(ext.clone(), dll);
            }
            let bytes = rasm::write_pe(&module, &map, "main")?;
            let info = format!("exe: {} bytes, imports {:?}", bytes.len(), map);
            Ok((bytes, info))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn the language thread, or skip the test if the db isn't present here.
    fn lang() -> Option<Lang> {
        let db = std::env::var("WINKB_DB")
            .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
        Lang::spawn(&db).ok()
    }

    #[test]
    fn card_function() {
        let Some(l) = lang() else { return };
        match l.card("CreateFileW") {
            Some(Response::Card { markdown, .. }) => assert!(markdown.contains("CreateFileW")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn card_struct() {
        let Some(l) = lang() else { return };
        match l.card("RECT") {
            Some(Response::Card { markdown, .. }) => assert!(markdown.contains("sizeof")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn card_search_fallback() {
        let Some(l) = lang() else { return };
        match l.card("CreateFile") {
            Some(Response::Card { markdown, .. }) => assert!(markdown.contains("was:")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn frame_has_widgets() {
        let Some(l) = lang() else { return };
        match l.frame("CreateFileW") {
            Some(Response::Frame { line: Some(s), .. }) => assert!(s.contains("{{select:")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn frame_unknown_is_none() {
        let Some(l) = lang() else { return };
        match l.frame("NoSuchFunctionXyzzy") {
            Some(Response::Frame { line, .. }) => assert!(line.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn check_flags_bad_arity() {
        let Some(l) = lang() else { return };
        let src = ".globl main\nmain:\n  invoke MessageBoxW, 0, 0\n  ret\n";
        match l.check_src(src) {
            Some(Response::Check { diags, .. }) => {
                assert!(diags.iter().any(|d| d.message.contains("argument")), "{diags:?}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn check_clean_source_is_empty() {
        let Some(l) = lang() else { return };
        let src = ".globl main\nmain:\n  mov eax, 5\n  ret\n";
        match l.check_src(src) {
            Some(Response::Check { diags, .. }) => assert!(diags.is_empty(), "{diags:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn assemble_obj_is_coff_amd64() {
        let Some(l) = lang() else { return };
        let src = ".globl main\nmain:\n  invoke ExitProcess, 7\n  ret\n";
        match l.assemble(src, Emit::Obj) {
            Some(Response::Assembled { bytes, emit, .. }) => {
                assert_eq!(emit, Emit::Obj);
                // COFF header Machine field = IMAGE_FILE_MACHINE_AMD64 (0x8664 LE).
                assert_eq!(&bytes[0..2], &[0x64, 0x86], "COFF machine");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn assemble_exe_is_pe() {
        let Some(l) = lang() else { return };
        let src = ".globl main\nmain:\n  invoke ExitProcess, 7\n  ret\n";
        match l.assemble(src, Emit::Exe) {
            Some(Response::Assembled { bytes, .. }) => assert_eq!(&bytes[0..2], b"MZ"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn assemble_asm_passes_through_lowered_text() {
        let Some(l) = lang() else { return };
        let src = ".globl main\nmain:\n  invoke ExitProcess, 7\n  ret\n";
        match l.assemble(src, Emit::Asm) {
            Some(Response::Assembled { bytes, emit, .. }) => {
                assert_eq!(emit, Emit::Asm);
                let txt = String::from_utf8(bytes).unwrap();
                assert!(txt.contains("ExitProcess"), "{txt}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn assemble_error_is_reported_not_fatal() {
        let Some(l) = lang() else { return };
        let src = "main:\n  this_is_not_an_instruction rax, rbx\n";
        match l.assemble(src, Emit::Obj) {
            Some(Response::Error { message, .. }) => assert!(!message.is_empty()),
            other => panic!("expected an error, got {other:?}"),
        }
        // The worker survives the error: a follow-up request still works.
        assert!(matches!(l.card("RECT"), Some(Response::Card { .. })));
    }

    #[test]
    fn suggest_did_you_mean() {
        let Some(l) = lang() else { return };
        match l.suggest("OPEN_EXISTNG") {
            Some(Response::Suggest { names, .. }) => {
                assert!(names.iter().any(|n| n == "OPEN_EXISTING"), "{names:?}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn async_replies_correlate_by_id() {
        let Some(l) = lang() else { return };
        let a = l.post_card("RECT");
        let b = l.post_suggest("OPEN_EXISTNG");
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2 {
            let r = l.recv_timeout(5000).expect("a reply");
            seen.insert(r.id());
        }
        assert!(seen.contains(&a) && seen.contains(&b), "ids {a},{b} seen {seen:?}");
    }

    #[test]
    fn poll_is_nonblocking_when_idle() {
        let Some(l) = lang() else { return };
        assert!(l.poll().is_none());
    }

    #[test]
    fn shutdown_joins_cleanly() {
        let Some(l) = lang() else { return };
        l.shutdown();
    }
}
