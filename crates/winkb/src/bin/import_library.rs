//! import_library — (re)build the WRASM standard-library symbol index.
//!
//!   import_library [root …]
//!
//! Incrementally syncs the `library_symbols` (+ `library_files`) tables of
//! `windows_api.db` with the public `module` symbols under the roots — default
//! `$WRASMLIB/library` + `$WRASMLIB/gpu` (or `library` + `gpu` relative to the
//! CWD when `$WRASMLIB` is unset). All the work is in [`winkb::library::sync`],
//! which studio's watcher thread calls too — one code path, no drift. The db is
//! switched to WAL so a read-only `Kb` keeps querying during the write.
//! DB: $WINKB_DB else E:\windows_api\windows_api.db.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let roots: Vec<PathBuf> = if args.is_empty() {
        winkb::library::default_roots()
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    let db = std::env::var("WINKB_DB").unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());

    let conn = Connection::open(&db).with_context(|| format!("open {db}"))?;
    let r = winkb::library::sync(&roots, &conn).context("sync library index")?;

    println!(
        "library index: {} public symbols ({} files changed, {} removed) from roots {:?} in {db}",
        r.symbols, r.changed, r.removed, roots
    );
    Ok(())
}
