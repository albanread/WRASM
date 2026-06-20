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

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use winkb::{Completion, Kb};

use crate::complete::{self, CompletionKind};

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

/// One instruction in the lowered listing: its bytes, a per-byte "unresolved"
/// mask (reloc placeholders the editor renders as `??`), and the asm text.
pub type ListingRow = (Vec<u8>, Vec<bool>, String);

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
    /// Autocomplete candidates for `line` at byte offset `cursor`. `binds` are the
    /// buffer's `comobj` name→interface bindings (for typed-pointer completion).
    Complete { id: u64, line: String, cursor: usize, binds: Vec<(String, String)> },
    /// Tooltip for the token under the caret (`line` at byte offset `cursor`).
    Hover { id: u64, line: String, cursor: usize },
    /// Signature help for an `invoke` at `line`/`cursor`.
    Signature { id: u64, line: String, cursor: usize },
    /// The machine-code bytes for a single source line (the live-bytes view).
    LineBytes { id: u64, line: String },
    /// The lowered listing of a whole buffer, grouped per source line — each
    /// expanded instruction with its bytes (the macro / `.while`-expansion view).
    /// Whole-buffer, not per-line, because block constructs are stateful across
    /// lines (a `.while` only resolves once its `.endw` is in the same lowering).
    Listing { id: u64, src: String },
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
    /// Autocomplete candidates plus the byte offset where the replaced prefix
    /// begins (`line[replace_start..cursor]` is what a choice replaces).
    Completions { id: u64, items: Vec<Completion>, replace_start: usize },
    /// Hover / signature tooltip markdown, or `None` if there's nothing to show.
    Tip { id: u64, markdown: Option<String> },
    /// The bytes a single line encodes to (empty if it isn't self-encodable,
    /// e.g. a label-only line or one that references an undefined label).
    LineBytes { id: u64, bytes: Vec<u8> },
    /// Per source line, each lowered instruction as a [`ListingRow`].
    Listing { id: u64, rows: Vec<Vec<ListingRow>> },
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
            | Response::Completions { id, .. }
            | Response::Tip { id, .. }
            | Response::LineBytes { id, .. }
            | Response::Listing { id, .. }
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
    /// Replies a blocking [`call`](Lang::call) pulled off `rx` while waiting for a
    /// *different* id — buffered here instead of dropped, so an async reply that
    /// happens to land mid-`call` is still delivered by a later [`poll`](Lang::poll)
    /// / [`recv_timeout`](Lang::recv_timeout). This is what lets the sync and async
    /// styles share one `Lang` without losing messages. Single-thread interior
    /// mutability: `Lang` lives on (and is only touched from) the GUI thread.
    pending: RefCell<VecDeque<Response>>,
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
                pending: RefCell::new(VecDeque::new()),
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
    pub fn post_listing(&self, src: &str) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Listing { id, src: src.to_string() });
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
    pub fn post_complete(&self, line: &str, cursor: usize, binds: Vec<(String, String)>) -> u64 {
        let id = self.alloc();
        let _ = self.tx.send(Request::Complete { id, line: line.to_string(), cursor, binds });
        id
    }

    /// Non-blocking drain — the GUI calls this each message-loop pass. Replies a
    /// prior [`call`](Lang::call) buffered come out first, in arrival order.
    pub fn poll(&self) -> Option<Response> {
        if let Some(r) = self.pending.borrow_mut().pop_front() {
            return Some(r);
        }
        self.rx.try_recv().ok()
    }

    /// Block for the next reply up to `ms` milliseconds (buffered replies first).
    pub fn recv_timeout(&self, ms: u64) -> Option<Response> {
        if let Some(r) = self.pending.borrow_mut().pop_front() {
            return Some(r);
        }
        self.rx.recv_timeout(Duration::from_millis(ms)).ok()
    }

    // ── Sync style: post and wait for the matching reply ─────────────────────

    /// Post a request and block for *its* reply (matching id), with the
    /// WF66-standard 5-second guard. Returns `None` on timeout or a dead worker.
    ///
    /// Replies for *other* (async) ids that arrive while we wait are buffered for
    /// the next [`poll`](Lang::poll)/[`recv_timeout`](Lang::recv_timeout) rather
    /// than discarded — so interleaving a sync `call` with async `post_*` never
    /// loses a reply.
    pub fn call(&self, make: impl FnOnce(u64) -> Request) -> Option<Response> {
        let id = self.alloc();
        self.tx.send(make(id)).ok()?;
        loop {
            match self.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(r) if r.id() == id => return Some(r),
                Ok(r) => self.pending.borrow_mut().push_back(r), // keep it for poll()
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
    pub fn complete(&self, line: &str, cursor: usize, binds: Vec<(String, String)>) -> Option<Response> {
        self.call(|id| Request::Complete { id, line: line.to_string(), cursor, binds: binds.clone() })
    }
    pub fn hover(&self, line: &str, cursor: usize) -> Option<Response> {
        self.call(|id| Request::Hover { id, line: line.to_string(), cursor })
    }
    pub fn signature(&self, line: &str, cursor: usize) -> Option<Response> {
        self.call(|id| Request::Signature { id, line: line.to_string(), cursor })
    }
    pub fn line_bytes(&self, line: &str) -> Option<Response> {
        self.call(|id| Request::LineBytes { id, line: line.to_string() })
    }
    pub fn listing(&self, src: &str) -> Option<Response> {
        self.call(|id| Request::Listing { id, src: src.to_string() })
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

    // Process requests in bursts: drain everything already queued, drop the
    // per-keystroke "live read" requests a newer same-kind request supersedes,
    // then answer the survivors in order. A `Shutdown` anywhere in the burst ends
    // the loop. Sync `call`s are unaffected — they post one request and block, so
    // their burst is a singleton and nothing coalesces.
    while let Ok(first) = rx.recv() {
        if matches!(first, Request::Shutdown) {
            break;
        }
        let mut batch = vec![first];
        let mut shutdown = false;
        loop {
            match rx.try_recv() {
                Ok(Request::Shutdown) => {
                    shutdown = true;
                    break;
                }
                Ok(more) => batch.push(more),
                Err(_) => break,
            }
        }
        for req in coalesce_superseded(batch) {
            if tx.send(handle(&kb, req)).is_err() {
                return; // the GUI handle was dropped; nothing left to answer
            }
        }
        if shutdown {
            break;
        }
    }
}

/// A coarse tag for the per-keystroke "live read" requests that newer input
/// supersedes — only the latest of each in a burst is worth doing. Explicit
/// actions (`Card`/`Frame`/`Assemble`/`Suggest`) and `Shutdown` return `None` and
/// are never coalesced away.
fn coalesce_tag(req: &Request) -> Option<u8> {
    match req {
        Request::Check { .. } => Some(0),
        Request::Complete { .. } => Some(1),
        Request::Hover { .. } => Some(2),
        Request::Signature { .. } => Some(3),
        Request::LineBytes { .. } => Some(4),
        Request::Listing { .. } => Some(5),
        _ => None,
    }
}

/// Drop every coalescable request that a *later* request of the same kind in the
/// burst supersedes, preserving order and all non-coalescable requests. The last
/// `Complete` of a typing burst survives; the stale ones before it never run.
fn coalesce_superseded(batch: Vec<Request>) -> Vec<Request> {
    let keep: Vec<bool> = batch
        .iter()
        .enumerate()
        .map(|(i, req)| match coalesce_tag(req) {
            Some(t) => !batch[i + 1..].iter().any(|r| coalesce_tag(r) == Some(t)),
            None => true,
        })
        .collect();
    batch
        .into_iter()
        .zip(keep)
        .filter_map(|(req, k)| k.then_some(req))
        .collect()
}

/// Answer one request. Never panics on a backend error — every failure becomes a
/// [`Response::Error`] so the worker loop stays alive. `Shutdown` is handled by
/// the loop and never reaches here.
fn handle(kb: &Kb, req: Request) -> Response {
    match req {
        Request::Shutdown => unreachable!("Shutdown is handled by the worker loop"),
        Request::Card { id, query } => match ide::answer(kb, &query) {
            Ok(markdown) => Response::Card { id, markdown },
            Err(e) => Response::Error { id, message: format!("{e:#}") },
        },
        Request::Frame { id, func } => match ide::insert_frame(kb, &func) {
            Ok(line) => Response::Frame { id, line },
            Err(e) => Response::Error { id, message: format!("{e:#}") },
        },
        Request::Check { id, src } => {
            let diags = was::check(&src, kb)
                .into_iter()
                .map(|d| Diag { line: d.line, col: d.col, message: d.message })
                .collect();
            Response::Check { id, diags }
        }
        Request::Assemble { id, src, emit } => match assemble_bytes(kb, &src, emit) {
            Ok((bytes, info)) => Response::Assembled { id, emit, bytes, info },
            Err(e) => Response::Error { id, message: format!("{e:#}") },
        },
        Request::Suggest { id, name } => match kb.suggest(&name, 5) {
            Ok(names) => Response::Suggest { id, names },
            Err(e) => Response::Error { id, message: format!("{e:#}") },
        },
        Request::Complete { id, line, cursor, binds } => {
            let ctx = complete::context(&line, cursor);
            Response::Completions {
                id,
                items: complete_items(kb, &ctx, &binds),
                replace_start: ctx.start,
            }
        }
        Request::Hover { id, line, cursor } => {
            let markdown = crate::hover::token_at(&line, cursor)
                .and_then(|t| hover_markdown(kb, &line[t.start..t.end]));
            Response::Tip { id, markdown }
        }
        Request::Signature { id, line, cursor } => {
            let markdown = crate::sig::active_param(&line, cursor)
                .and_then(|(func, active)| signature_markdown(kb, &func, active));
            Response::Tip { id, markdown }
        }
        Request::LineBytes { id, line } => {
            // A line that can't stand alone (label-only, or referencing an
            // undefined label) just shows no bytes — not an error popup.
            let bytes = was::lower(&line, kb)
                .and_then(|asm| rasm::assemble(&asm))
                .map(|m| m.code)
                .unwrap_or_default();
            Response::LineBytes { id, bytes }
        }
        Request::Listing { id, src } => Response::Listing { id, rows: buffer_listing(kb, &src) },
    }
}

/// Lower the whole buffer once and group the result per source line: for each
/// source line, the `(bytes, unresolved, asm)` of every instruction it lowered
/// to. Lowering the whole buffer (not each line alone) lets stateful constructs
/// — `.while`/`.endw` and their matched labels — resolve, and assembling it whole
/// gives *real* branch displacements (relaxed `rel8`/`rel32`, resolved offsets);
/// only true externs stay as reloc placeholders, flagged for `??`. If the buffer
/// can't assemble yet (mid-edit), fall back to per-instruction isolation.
fn buffer_listing(kb: &Kb, src: &str) -> Vec<Vec<ListingRow>> {
    let n = src.lines().count();
    let mut out = vec![Vec::new(); n];
    let Ok((lowered, src_map)) = was::lower_mapped(src, kb) else {
        return out;
    };
    let Ok((module, spans)) = rasm::assemble_listing(&lowered) else {
        return isolated_listing(&lowered, &src_map, n);
    };
    let unresolved = reloc_mask(&module);
    for (i, line) in lowered.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        let Some(&src_line) = src_map.get(i) else { continue };
        if src_line == 0 || src_line > n {
            continue;
        }
        let (start, len) = spans.get(i).copied().unwrap_or((0, 0));
        let end = (start + len).min(module.code.len());
        let start = start.min(end);
        out[src_line - 1].push((
            module.code[start..end].to_vec(),
            unresolved[start..end].to_vec(),
            t.to_string(),
        ));
    }
    out
}

/// A per-byte mask of `code`: true where a relocation covers the byte (an extern
/// field whose value isn't known until link).
fn reloc_mask(m: &rasm::EncodedModule) -> Vec<bool> {
    let mut mask = vec![false; m.code.len()];
    for r in &m.relocs {
        let end = (r.at + r.size as usize).min(m.code.len());
        for b in &mut mask[r.at.min(end)..end] {
            *b = true;
        }
    }
    mask
}

/// Per-instruction fallback for when the whole buffer can't assemble: each
/// lowered instruction alone, with its reloc fields flagged unresolved (so a
/// branch to a label outside this one line shows `??`, not a misleading `00`).
fn isolated_listing(lowered: &str, src_map: &[usize], n: usize) -> Vec<Vec<ListingRow>> {
    let mut out = vec![Vec::new(); n];
    for (i, line) in lowered.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        let Some(&src_line) = src_map.get(i) else { continue };
        if src_line == 0 || src_line > n {
            continue;
        }
        let (bytes, mask) = match rasm::assemble(t) {
            Ok(m) => (m.code.clone(), reloc_mask(&m)),
            Err(_) => (Vec::new(), Vec::new()),
        };
        out[src_line - 1].push((bytes, mask, t.to_string()));
    }
    out
}

/// Resolve a completion context into winkb candidates.
fn complete_items(
    kb: &Kb,
    ctx: &complete::CompletionContext,
    binds: &[(String, String)],
) -> Vec<Completion> {
    match &ctx.kind {
        CompletionKind::None => Vec::new(),
        CompletionKind::Function => kb.complete(&ctx.prefix, "function", 50).unwrap_or_default(),
        CompletionKind::Symbol => kb.complete(&ctx.prefix, "all", 50).unwrap_or_default(),
        // `obj.` where obj is a typed `comobj` pointer → the interface's methods
        // (own + inherited). Otherwise a struct field.
        CompletionKind::Field { type_name } => {
            if let Some((_, iface)) = binds.iter().find(|(n, _)| n == type_name) {
                interface_methods(kb, iface)
                    .into_iter()
                    .filter(|(name, _)| name.starts_with(&ctx.prefix))
                    .map(|(name, slot)| Completion {
                        name,
                        kind: "method".into(),
                        detail: format!("vtbl[{slot}] {iface}"),
                    })
                    .collect()
            } else {
                kb.layout(type_name)
                    .ok()
                    .flatten()
                    .map(|l| {
                        l.fields
                            .into_iter()
                            .filter(|f| f.name.starts_with(&ctx.prefix))
                            .map(|f| Completion {
                                name: f.name,
                                kind: "field".into(),
                                detail: f.type_name,
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
        }
    }
}

/// Every method callable on `iface` as `(name, vtable_slot)`, walking the base
/// chain so inherited IUnknown/parent methods are offered too.
fn interface_methods(kb: &Kb, iface: &str) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let mut name = iface.to_string();
    for _ in 0..32 {
        let Ok(Some(i)) = kb.interface(&name) else { break };
        for m in &i.methods {
            out.push((m.name.clone(), m.vtable_index));
        }
        match i.base {
            Some(b) => name = b.rsplit('.').next().unwrap_or(&b).to_string(),
            None => break,
        }
    }
    out.sort_by_key(|(_, slot)| *slot);
    out
}

/// A concise tooltip for a token: a constant's value, a function's one-line
/// signature, or a type's size — whichever winkb knows. `None` if unknown.
fn hover_markdown(kb: &Kb, word: &str) -> Option<String> {
    if let Ok(vals) = kb.resolve(word) {
        if let Some(v) = vals.first() {
            let ns = v.namespace.as_deref().unwrap_or("");
            return Some(format!("**{word}** = `0x{:x}` ({})  ·  _{ns}_", v.bits, v.i64v));
        }
    }
    if let Ok(Some(f)) = kb.function(word) {
        let dll = f.dll.as_deref().unwrap_or("?");
        return Some(format!(
            "`{}` **{}**({} param{})  ·  {dll}",
            f.ret,
            f.name,
            f.params.len(),
            if f.params.len() == 1 { "" } else { "s" },
        ));
    }
    if let Ok(Some(l)) = kb.layout(word) {
        return Some(format!("**{}**  ·  {}, sizeof `{}`", l.name, l.kind, l.size));
    }
    None
}

/// The signature of `func` with parameter `active` bolded — the signature-help
/// tooltip. `None` if the function is unknown.
fn signature_markdown(kb: &Kb, func: &str, active: usize) -> Option<String> {
    let f = kb.function(func).ok().flatten()?;
    let params: Vec<String> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let one = format!("{} {}", p.type_name, p.name);
            if i == active {
                format!("**{one}**")
            } else {
                one
            }
        })
        .collect();
    Some(format!("`{}` {}({})", f.ret, f.name, params.join(", ")))
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
    fn complete_after_invoke_lists_functions() {
        let Some(l) = lang() else { return };
        let line = "invoke CreateFil";
        match l.complete(line, line.len(), vec![]) {
            Some(Response::Completions { items, replace_start, .. }) => {
                assert_eq!(replace_start, 7, "replaces the typed name");
                assert!(items.iter().any(|c| c.name == "CreateFileW"), "{items:?}");
                assert!(items.iter().all(|c| c.kind == "function"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn complete_struct_field_after_dot() {
        let Some(l) = lang() else { return };
        let line = "mov eax, [rcx + RECT.";
        match l.complete(line, line.len(), vec![]) {
            Some(Response::Completions { items, .. }) => {
                let names: Vec<_> = items.iter().map(|c| c.name.as_str()).collect();
                assert!(names.contains(&"left") && names.contains(&"right"), "{names:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn complete_after_typed_pointer_lists_interface_methods() {
        let Some(l) = lang() else { return };
        // `pSwap` is a typed pointer to IDXGISwapChain → `pSwap.` offers methods.
        let binds = vec![("pSwap".to_string(), "IDXGISwapChain".to_string())];
        match l.complete("  pSwap.", 8, binds) {
            Some(Response::Completions { items, .. }) => {
                assert!(items.iter().any(|c| c.name == "Present"), "own method: {items:?}");
                assert!(items.iter().any(|c| c.name == "Release"), "inherited method: {items:?}");
                assert!(items.iter().all(|c| c.kind == "method"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn complete_offers_nothing_for_the_mnemonic() {
        let Some(l) = lang() else { return };
        match l.complete("mo", 2, vec![]) {
            Some(Response::Completions { items, .. }) => assert!(items.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hover_resolves_a_constant_value() {
        let Some(l) = lang() else { return };
        let line = "mov eax, OPEN_EXISTING";
        match l.hover(line, 12) {
            Some(Response::Tip { markdown: Some(md), .. }) => {
                assert!(md.contains("OPEN_EXISTING") && md.contains("0x"), "{md}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hover_on_a_register_is_empty() {
        let Some(l) = lang() else { return };
        match l.hover("mov rax, 1", 4) {
            Some(Response::Tip { markdown, .. }) => assert!(markdown.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn signature_marks_the_active_param() {
        let Some(l) = lang() else { return };
        // caret after the first comma -> param 0 (lpFileName) is active/bold.
        let line = "invoke CreateFileW, rcx";
        match l.signature(line, line.len()) {
            Some(Response::Tip { markdown: Some(md), .. }) => {
                assert!(md.contains("CreateFileW"), "{md}");
                assert!(md.contains("**"), "an active param is marked: {md}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn line_bytes_encodes_a_single_instruction() {
        let Some(l) = lang() else { return };
        match l.line_bytes("mov rax, 5") {
            Some(Response::LineBytes { bytes, .. }) => {
                // REX.W mov r64, imm32 -> 48 c7 c0 05 00 00 00
                assert_eq!(bytes, vec![0x48, 0xc7, 0xc0, 0x05, 0x00, 0x00, 0x00]);
            }
            other => panic!("{other:?}"),
        }
        match l.line_bytes("ret") {
            Some(Response::LineBytes { bytes, .. }) => assert_eq!(bytes, vec![0xc3]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn line_bytes_blank_for_a_label_only_line() {
        let Some(l) = lang() else { return };
        match l.line_bytes("main:") {
            Some(Response::LineBytes { bytes, .. }) => assert!(bytes.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn line_bytes_encodes_an_invoke() {
        let Some(l) = lang() else { return };
        match l.line_bytes("invoke ExitProcess, 7") {
            Some(Response::LineBytes { bytes, .. }) => assert!(!bytes.is_empty()),
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

    /// A blocking `call` that pulls an async reply off the channel while waiting
    /// for its own must not drop it — it stays available for a later drain.
    #[test]
    fn sync_call_preserves_a_pending_async_reply() {
        let Some(l) = lang() else { return };
        let async_id = l.post_card("RECT"); // fire-and-forget
        // A sync call whose worker reply may arrive after the async one.
        assert!(matches!(l.suggest("OPEN_EXISTNG"), Some(Response::Suggest { .. })));
        // The async reply must still be retrievable (buffered or still on rx).
        let mut found = false;
        for _ in 0..3 {
            if let Some(r) = l.recv_timeout(5000) {
                if r.id() == async_id {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "async card reply id {async_id} was lost across a sync call()");
    }

    fn req_id(r: &Request) -> u64 {
        match r {
            Request::Card { id, .. }
            | Request::Frame { id, .. }
            | Request::Check { id, .. }
            | Request::Assemble { id, .. }
            | Request::Suggest { id, .. }
            | Request::Complete { id, .. }
            | Request::Hover { id, .. }
            | Request::Signature { id, .. }
            | Request::LineBytes { id, .. }
            | Request::Listing { id, .. } => *id,
            Request::Shutdown => 0,
        }
    }

    #[test]
    fn coalesce_keeps_last_live_read_drops_earlier() {
        let batch = vec![
            Request::Complete { id: 1, line: "a".into(), cursor: 1, binds: vec![] },
            Request::Card { id: 2, query: "RECT".into() },
            Request::Complete { id: 3, line: "ab".into(), cursor: 2, binds: vec![] },
            Request::Hover { id: 4, line: "x".into(), cursor: 0 },
        ];
        let ids: Vec<u64> = coalesce_superseded(batch).iter().map(req_id).collect();
        // Complete#1 superseded by #3; Card and the lone Hover survive; order kept.
        assert_eq!(ids, vec![2, 3, 4]);
    }

    #[test]
    fn coalesce_never_drops_explicit_actions() {
        let batch = vec![
            Request::Card { id: 1, query: "A".into() },
            Request::Card { id: 2, query: "B".into() },
            Request::Assemble { id: 3, src: String::new(), emit: Emit::Obj },
        ];
        let ids: Vec<u64> = coalesce_superseded(batch).iter().map(req_id).collect();
        assert_eq!(ids, vec![1, 2, 3], "explicit actions are never coalesced");
    }

    #[test]
    fn coalesce_collapses_a_typing_burst_to_the_latest() {
        let batch: Vec<Request> = (0..5)
            .map(|i| Request::Complete { id: i, line: "x".repeat(i as usize + 1), cursor: 1, binds: vec![] })
            .collect();
        let kept = coalesce_superseded(batch);
        assert_eq!(kept.len(), 1);
        assert_eq!(req_id(&kept[0]), 4, "only the newest keystroke survives");
    }
}
