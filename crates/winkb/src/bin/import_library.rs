//! import_library — sweep the WRASM standard library's `module … endmodule`
//! regions and index every PUBLIC symbol (capitalised name) into the
//! `library_symbols` table of `windows_api.db`.
//!
//!   import_library [root …]        (default roots: library gpu)
//!
//! For each `.was`/`.inc` under the roots, [`winkb::library::scan_was`] extracts
//! the public symbols — module, file:line, kind, a `proc`'s in/out/uses
//! signature, and the doc comment above it. The table is rebuilt from scratch.
//! DB: $WINKB_DB else E:\windows_api\windows_api.db. Run from the repo root.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let roots: Vec<String> = if args.is_empty() {
        vec!["library".into(), "gpu".into()]
    } else {
        args
    };
    let db = std::env::var("WINKB_DB").unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());

    let mut syms = Vec::new();
    let mut files = 0usize;
    for root in &roots {
        scan_dir(Path::new(root), &mut syms, &mut files)?;
    }

    let conn = Connection::open(&db).with_context(|| format!("open {db}"))?;
    conn.execute_batch(
        "DROP TABLE IF EXISTS library_symbols;
         CREATE TABLE library_symbols (
            name      TEXT NOT NULL,
            module    TEXT NOT NULL,
            file      TEXT NOT NULL,
            line      INTEGER NOT NULL,
            kind      TEXT NOT NULL,
            signature TEXT NOT NULL,
            summary   TEXT NOT NULL
         );
         CREATE INDEX library_symbols_name ON library_symbols(name);",
    )?;

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO library_symbols (name, module, file, line, kind, signature, summary) \
               VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for s in &syms {
            stmt.execute(params![
                s.name, s.module, s.file, s.line as i64, s.kind, s.signature, s.summary
            ])?;
        }
    }
    tx.commit()?;

    let modules: std::collections::BTreeSet<&str> = syms.iter().map(|s| s.module.as_str()).collect();
    println!(
        "imported {} public symbols across {} modules ({}) from {} files in {:?} into {db}",
        syms.len(),
        modules.len(),
        modules.iter().copied().collect::<Vec<_>>().join(", "),
        files,
        roots,
    );
    Ok(())
}

/// Recurse `dir`, scanning each `.was`/`.inc` for public module symbols.
fn scan_dir(dir: &Path, out: &mut Vec<winkb::LibrarySymbol>, files: &mut usize) -> Result<()> {
    if !dir.exists() {
        eprintln!("skip {} (not found)", dir.display());
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            scan_dir(&path, out, files)?;
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("was") | Some("inc")) {
            let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let rel = path.to_string_lossy().replace('\\', "/");
            out.extend(winkb::library::scan_was(&rel, &text));
            *files += 1;
        }
    }
    Ok(())
}
