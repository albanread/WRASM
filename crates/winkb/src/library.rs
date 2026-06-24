//! Library symbol index — the public API of the WRASM standard library.
//!
//! The assembler's `module NAME … endmodule` regions give every label a
//! visibility: a name whose first letter is **uppercase is PUBLIC** (the module's
//! exported API); a lowercase/`_` name is private (mangled `Module$name`). A
//! module is independent of files — `module Canvas` is split across
//! `blit/canvas/fx/…`, pooling by name.
//!
//! [`scan_was`] sweeps one file's module regions for the public symbols and what
//! we can say about each — its module, location, kind, a `proc`'s in/out/uses
//! signature, and the doc comment above it. The `import_library` bin walks
//! `library/` + `gpu/` and writes them to the `library_symbols` table, which
//! [`crate::Kb::library_symbols`] reads back so the IDE can answer "where does
//! `Blit` come from?" for your own code, the way it already does for Windows.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// One public symbol defined inside a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySymbol {
    pub name: String,
    pub module: String,
    pub file: String,
    /// 1-based line of the definition within `file`.
    pub line: usize,
    /// `proc` | `data` | `equate` | `label` | `macro`.
    pub kind: String,
    /// For a `proc`, a readable `in … · out … · uses … · frame`; else empty.
    pub signature: String,
    /// The doc comment immediately above the definition (cleaned), or empty.
    pub summary: String,
    /// The definition's source — a `proc`'s body through `endproc` (capped), or
    /// the single definition line for data/equates. For the IDE's peek view.
    pub source: String,
}

/// Scan one `.was`/`.inc` file's `module … endmodule` regions for PUBLIC symbols
/// (first letter uppercase). `file` is stored verbatim with each symbol (use a
/// repo-relative, forward-slash path). Lines outside any module are ignored — the
/// public/private rule only exists inside modules.
pub fn scan_was(file: &str, text: &str) -> Vec<LibrarySymbol> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut module: Option<String> = None; // the module owning the current line

    for (i, raw) in lines.iter().enumerate() {
        let t = strip_comment(raw).trim();
        let mut words = t.split_whitespace();
        match words.next() {
            Some("module") => {
                module = words.next().map(str::to_string);
                continue;
            }
            Some("endmodule") => {
                module = None;
                continue;
            }
            _ => {}
        }
        let Some(m) = module.as_deref() else { continue };
        let Some((name, kind, signature)) = def_on_line(t) else { continue };
        if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            continue; // lowercase first letter → private
        }
        out.push(LibrarySymbol {
            name: name.to_string(),
            module: m.to_string(),
            file: file.to_string(),
            line: i + 1,
            kind: kind.to_string(),
            summary: doc_above(&lines, i),
            source: snippet(&lines, i, kind),
            signature,
        });
    }
    out
}

/// A symbol definition on `t`: `(name, kind, signature)`. Recognises `proc`,
/// `macro`, a `name:` label, `name equ/= value`, and a `name TYPE …` data slot.
fn def_on_line(t: &str) -> Option<(&str, &'static str, String)> {
    // proc NAME [frame] [uses R…] [in R…] [out R…]
    if let Some(rest) = strip_kw(t, "proc") {
        let name = rest.split_whitespace().next()?;
        let sig = proc_signature(rest[name.len()..].trim());
        return Some((name, "proc", sig));
    }
    if let Some(rest) = strip_kw(t, "macro") {
        return Some((rest.split_whitespace().next()?, "macro", String::new()));
    }
    // NAME:  (a label, optionally followed by an instruction)
    if let Some(name) = leading_label(t) {
        return Some((name, "label", String::new()));
    }
    let mut words = t.split_whitespace();
    let name = words.next().filter(|w| is_ident(w))?;
    let op = words.next()?;
    if op.eq_ignore_ascii_case("equ") || op == "=" {
        let value = t[name.len()..].trim_start();
        let value = strip_kw(value, "equ").unwrap_or_else(|| value.trim_start_matches('=').trim());
        return Some((name, "equate", value.trim().to_string()));
    }
    // NAME <TYPE> …  — but not a `dword ptr` size override (that's an operand).
    if is_data_type(op) && words.next() != Some("ptr") {
        return Some((name, "data", op.to_ascii_uppercase()));
    }
    None
}

/// Build a readable signature from a `proc` header's tail (after the name).
fn proc_signature(tail: &str) -> String {
    let (mut frame, mut bucket) = (false, 0u8);
    let (mut uses, mut ins, mut outs) = (Vec::new(), Vec::new(), Vec::new());
    for tok in tail.split_whitespace() {
        match tok {
            "frame" => frame = true,
            "uses" => bucket = 1,
            "in" => bucket = 2,
            "out" => bucket = 3,
            r => match bucket {
                1 => uses.push(r),
                2 => ins.push(r),
                3 => outs.push(r),
                _ => {}
            },
        }
    }
    let mut parts = Vec::new();
    if !ins.is_empty() {
        parts.push(format!("in {}", ins.join(" ")));
    }
    if !outs.is_empty() {
        parts.push(format!("out {}", outs.join(" ")));
    }
    if !uses.is_empty() {
        parts.push(format!("uses {}", uses.join(" ")));
    }
    if frame {
        parts.push("frame".to_string());
    }
    parts.join(" · ")
}

/// The cleaned doc comment immediately above line `i` (the contiguous run of `;`
/// lines, stopping at a blank/code line or a pure separator rule).
fn doc_above(lines: &[&str], i: usize) -> String {
    let mut block: Vec<String> = Vec::new();
    let mut j = i;
    while j > 0 {
        j -= 1;
        let l = lines[j].trim();
        let Some(c) = l.strip_prefix(';') else { break };
        let c = c.trim().trim_matches(|ch| ch == '-' || ch == '=' || ch == ' ');
        if c.is_empty() {
            break; // a separator / blank comment ends the doc block
        }
        block.push(c.to_string());
    }
    block.reverse();
    block.join(" ")
}

/// The definition's source for the peek view: a `proc` body through its
/// `endproc` (capped at `MAX` lines), else just the definition line.
fn snippet(lines: &[&str], i: usize, kind: &str) -> String {
    const MAX: usize = 48;
    if kind != "proc" {
        return lines[i].trim_end().to_string();
    }
    let mut end = i;
    for (k, raw) in lines.iter().enumerate().skip(i + 1).take(MAX * 4) {
        end = k;
        if strip_comment(raw).trim() == "endproc" {
            break;
        }
        if k - i >= MAX {
            break; // runaway / missing endproc guard
        }
    }
    lines[i..=end].iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
}

// ── small syntax helpers (kept self-contained so winkb stays a leaf crate) ──

/// `kw` followed by whitespace at the start of `t` → the remainder, else None.
fn strip_kw<'a>(t: &'a str, kw: &str) -> Option<&'a str> {
    let rest = t.strip_prefix(kw)?;
    rest.starts_with(|c: char| c.is_whitespace()).then(|| rest.trim_start())
}

/// `NAME:` at the start of `t` → NAME (a label definition).
fn leading_label(t: &str) -> Option<&str> {
    let (head, _) = t.split_once(':')?;
    let head = head.trim();
    (is_ident(head) && !head.is_empty()).then_some(head)
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '.')
        && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$')
}

fn is_data_type(s: &str) -> bool {
    matches!(
        s.trim_start_matches('.').to_ascii_lowercase().as_str(),
        "byte" | "sbyte" | "word" | "sword" | "dword" | "sdword" | "qword" | "sqword" | "tbyte"
            | "wchar" | "db" | "dw" | "dd" | "dq" | "dt" | "real4" | "real8" | "real10" | "single"
            | "double"
    )
}

/// Drop a trailing `;` line comment (respecting nothing fancy — library source
/// has no `;` inside strings on definition lines).
fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(i) => &line[..i],
        None => line,
    }
}

// ── incremental sync into the database ──────────────────────────────────────

/// What a [`sync`] changed.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncReport {
    /// Files seen on disk under the roots.
    pub scanned: usize,
    /// Files (re-)swept because their contents changed.
    pub changed: usize,
    /// Files pruned because they vanished from disk.
    pub removed: usize,
    /// Total public symbols indexed afterwards.
    pub symbols: usize,
}

impl SyncReport {
    /// Whether anything actually changed (so callers can skip a UI refresh).
    pub fn is_dirty(&self) -> bool {
        self.changed > 0 || self.removed > 0
    }
}

/// The default library roots: `$WRASMLIB/library` + `$WRASMLIB/gpu`, or just
/// `library` + `gpu` (relative to the CWD) when `$WRASMLIB` is unset.
pub fn default_roots() -> Vec<PathBuf> {
    let base: PathBuf = std::env::var_os("WRASMLIB").map(PathBuf::from).unwrap_or_default();
    vec![base.join("library"), base.join("gpu")]
}

/// Bring the `library_symbols` index up to date with the `.was`/`.inc` files
/// under `roots`, incrementally: a file is re-swept only when its `(mtime, size)`
/// *and* content hash changed; vanished files are pruned. Enables WAL so a
/// read-only [`crate::Kb`] keeps querying (and sees each commit) while this
/// writes. `conn` must be writable.
pub fn sync(roots: &[PathBuf], conn: &Connection) -> rusqlite::Result<SyncReport> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    ensure_schema(conn)?;

    let mut on_disk = Vec::new();
    for root in roots {
        collect(root, &mut on_disk);
    }

    let mut report = SyncReport::default();
    let mut seen = std::collections::HashSet::new();
    let tx = conn.unchecked_transaction()?;

    for path in &on_disk {
        let key = norm(path);
        seen.insert(key.clone());
        report.scanned += 1;
        let Ok(meta) = std::fs::metadata(path) else { continue };
        let (size, mtime) = (meta.len() as i64, mtime_secs(&meta));

        let prior: Option<(i64, i64, i64)> = tx
            .query_row("SELECT mtime, size, hash FROM library_files WHERE path=?1", [&key], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        if let Some((pm, ps, _)) = prior {
            if pm == mtime && ps == size {
                continue; // stat-gate: untouched, no read
            }
        }
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let hash = fnv1a(text.as_bytes()) as i64;
        if let Some((_, _, ph)) = prior {
            if ph == hash {
                // touched but byte-identical (e.g. a checkout) — refresh stat only.
                tx.execute("UPDATE library_files SET mtime=?1, size=?2 WHERE path=?3", params![mtime, size, key])?;
                continue;
            }
        }
        reindex_file(&tx, &key, &text)?;
        tx.execute(
            "INSERT INTO library_files(path, mtime, size, hash) VALUES(?1,?2,?3,?4) \
             ON CONFLICT(path) DO UPDATE SET mtime=?2, size=?3, hash=?4",
            params![key, mtime, size, hash],
        )?;
        report.changed += 1;
    }

    // Prune files that are indexed but no longer on disk — but ONLY when at least
    // one root directory actually exists. If none do (a wrong CWD, or `$WRASMLIB`
    // unset so the relative `library`/`gpu` don't resolve), `seen` is empty for the
    // wrong reason and pruning would wipe a perfectly good index. Skip it.
    if roots.iter().any(|r| r.exists()) {
        let stale: Vec<String> = {
            let mut stmt = tx.prepare("SELECT path FROM library_files")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(Result::ok).filter(|p| !seen.contains(p)).collect()
        };
        for p in &stale {
            tx.execute("DELETE FROM library_symbols WHERE file=?1", [p])?;
            tx.execute("DELETE FROM library_files WHERE path=?1", [p])?;
            report.removed += 1;
        }
    }

    report.symbols = tx.query_row("SELECT COUNT(*) FROM library_symbols", [], |r| r.get::<_, i64>(0))? as usize;
    tx.commit()?;
    Ok(report)
}

/// Open `db_path` writable and [`sync`] it — the entry point for callers (like
/// studio's watcher thread) that shouldn't depend on rusqlite directly.
pub fn sync_db(db_path: &str, roots: &[PathBuf]) -> rusqlite::Result<SyncReport> {
    let conn = Connection::open(db_path)?;
    // Wait out a brief writer (e.g. the import_library CLI) instead of failing the
    // whole tick with SQLITE_BUSY the instant the WAL write lock is contended.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    sync(roots, &conn)
}

/// Replace one file's symbols with a fresh sweep.
fn reindex_file(conn: &Connection, file: &str, text: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM library_symbols WHERE file=?1", [file])?;
    let mut stmt = conn.prepare(
        "INSERT INTO library_symbols(name, module, file, line, kind, signature, summary, source) \
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    for s in scan_was(file, text) {
        stmt.execute(params![
            s.name, s.module, s.file, s.line as i64, s.kind, s.signature, s.summary, s.source
        ])?;
    }
    Ok(())
}

/// Create the tables if absent; rebuild from scratch if an older schema (no
/// `source` column) is found.
fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='library_symbols'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    let has_source: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('library_symbols') WHERE name='source'")
        .and_then(|mut s| s.exists([]))
        .unwrap_or(false);
    if exists && !has_source {
        conn.execute_batch("DROP TABLE IF EXISTS library_symbols; DROP TABLE IF EXISTS library_files;")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS library_symbols (
            name TEXT NOT NULL, module TEXT NOT NULL, file TEXT NOT NULL, line INTEGER NOT NULL,
            kind TEXT NOT NULL, signature TEXT NOT NULL, summary TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT '');
         CREATE INDEX IF NOT EXISTS library_symbols_name ON library_symbols(name);
         CREATE TABLE IF NOT EXISTS library_files (
            path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, size INTEGER NOT NULL, hash INTEGER NOT NULL);",
    )
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("was") | Some("inc")) {
            out.push(p);
        }
    }
}

fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn mtime_secs(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// FNV-1a 64-bit — small, dependency-free, and stable across runs (so a hash
/// stored last session is comparable this session).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_public_module_symbols_with_signatures_and_docs() {
        let src = "\
module Canvas
.DATA
dstBase   QWORD ?            ; private — lowercase, skipped
Palette   DWORD 0           ; public data
.CODE
; ---- Blit: opaque rectangular copy ----
proc Blit frame in rcx rdx r8 r9
    ret
endproc
; a private helper
proc blitEx
    ret
endproc
endmodule
mov dword ptr [rip + x], 1   ; outside any module — ignored
";
        let syms = scan_was("library/blit.was", src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Palette", "Blit"], "only PUBLIC, in-module symbols");

        let blit = syms.iter().find(|s| s.name == "Blit").unwrap();
        assert_eq!(blit.module, "Canvas");
        assert_eq!(blit.file, "library/blit.was");
        assert_eq!(blit.kind, "proc");
        assert_eq!(blit.signature, "in rcx rdx r8 r9 · frame");
        assert_eq!(blit.summary, "Blit: opaque rectangular copy");
        assert!(
            blit.source.starts_with("proc Blit") && blit.source.ends_with("endproc"),
            "peek snippet is the proc body:\n{}",
            blit.source
        );

        let pal = syms.iter().find(|s| s.name == "Palette").unwrap();
        assert_eq!(pal.kind, "data");
        assert_eq!(pal.signature, "DWORD");
    }

    #[test]
    fn equates_and_a_dword_ptr_override() {
        let src = "module M\nCANVAS_W equ 320\n  mov dword ptr [rip + p], 1\nendmodule\n";
        let syms = scan_was("f.was", src);
        // CANVAS_W is a public equate; the `mov dword ptr` line defines nothing.
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].kind, "equate");
        assert_eq!(syms[0].signature, "320");
    }

    #[test]
    fn sync_is_incremental_and_prunes() {
        let dir = std::env::temp_dir().join(format!("winkb_sync_{}", std::process::id()));
        let lib = dir.join("library");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&lib).unwrap();
        let f = lib.join("m.was");
        std::fs::write(&f, "module M\nproc Alpha\n  ret\nendproc\n").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        let roots = vec![lib.clone()];

        let r1 = sync(&roots, &conn).unwrap();
        assert_eq!((r1.changed, r1.symbols), (1, 1), "first sweep");

        let r2 = sync(&roots, &conn).unwrap();
        assert_eq!(r2.changed, 0, "unchanged → stat-gate skips");

        // A bigger file (size differs) → re-swept; Beta now indexed too.
        std::fs::write(&f, "module M\nproc Alpha\n  ret\nendproc\nproc Beta\n  ret\nendproc\n").unwrap();
        let r3 = sync(&roots, &conn).unwrap();
        assert_eq!((r3.changed, r3.symbols), (1, 2), "edited → re-swept");

        std::fs::remove_file(&f).unwrap();
        let r4 = sync(&roots, &conn).unwrap();
        assert_eq!((r4.removed, r4.symbols), (1, 0), "vanished → pruned");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_does_not_prune_when_no_root_exists() {
        // A wrong CWD / unset $WRASMLIB must NOT be mistaken for "every file deleted".
        let dir = std::env::temp_dir().join(format!("winkb_prune_{}", std::process::id()));
        let lib = dir.join("library");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("m.was"), "module M\nproc Pub\n  ret\nendproc\n").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(sync(&[lib.clone()], &conn).unwrap().symbols, 1, "populated");

        // Sync against a root that doesn't exist → the index must survive intact.
        let r = sync(&[dir.join("nonexistent")], &conn).unwrap();
        assert_eq!((r.removed, r.symbols), (0, 1), "absent roots preserve the index");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
