//! import_instructions — load the clean-room instruction explainer table into
//! `windows_api.db` (the `instructions` table winkb's `instruction()` reads).
//!
//!   import_instructions [data.tsv]
//!
//! TSV columns (tab-separated, `#` comments and blank lines skipped):
//!   mnemonic  aliases  category  flags  summary  description
//! A row is written for the mnemonic and for each space-separated alias.
//!
//! Default data file: crates/winkb/data/instructions.tsv. DB: $WINKB_DB else
//! E:\windows_api\windows_api.db. The table is rebuilt from scratch each run.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let tsv = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "crates/winkb/data/instructions.tsv".to_string());
    let db = std::env::var("WINKB_DB")
        .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());

    let text = std::fs::read_to_string(&tsv).with_context(|| format!("read {tsv}"))?;
    let conn = Connection::open(&db).with_context(|| format!("open {db}"))?;

    conn.execute_batch(
        "DROP TABLE IF EXISTS instructions;
         CREATE TABLE instructions (
            mnemonic    TEXT PRIMARY KEY,
            category    TEXT NOT NULL,
            flags       TEXT NOT NULL,
            summary     TEXT NOT NULL,
            description TEXT NOT NULL
         );",
    )?;

    let tx = conn.unchecked_transaction()?;
    let mut rows = 0usize;
    let mut entries = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO instructions \
               (mnemonic, category, flags, summary, description) VALUES (?1,?2,?3,?4,?5)",
        )?;
        for (i, line) in text.lines().enumerate() {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 6 {
                eprintln!("skip line {}: {} fields (need 6)", i + 1, f.len());
                continue;
            }
            entries += 1;
            let mnem = f[0].trim().to_ascii_lowercase();
            let (category, flags, summary, description) =
                (f[2].trim(), f[3].trim(), f[4].trim(), f[5].trim());
            let mut names = vec![mnem];
            names.extend(f[1].split_whitespace().map(|a| a.to_ascii_lowercase()));
            for name in names {
                stmt.execute(params![name, category, flags, summary, description])?;
                rows += 1;
            }
        }
    }
    tx.commit()?;

    // ── WRASM dialect directives (a sibling table, same shape but `syntax`) ──
    let dpath = "crates/winkb/data/directives.tsv";
    let mut dirs = 0usize;
    if let Ok(dtext) = std::fs::read_to_string(dpath) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS directives;
             CREATE TABLE directives (
                name        TEXT PRIMARY KEY,
                category    TEXT NOT NULL,
                syntax      TEXT NOT NULL,
                summary     TEXT NOT NULL,
                description TEXT NOT NULL
             );",
        )?;
        let dtx = conn.unchecked_transaction()?;
        {
            let mut stmt = dtx.prepare(
                "INSERT OR REPLACE INTO directives \
                   (name, category, syntax, summary, description) VALUES (?1,?2,?3,?4,?5)",
            )?;
            for line in dtext.lines() {
                let line = line.trim_end_matches(['\r', '\n']);
                if line.trim().is_empty() || line.starts_with('#') {
                    continue;
                }
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() < 6 {
                    continue;
                }
                let mut names = vec![f[0].trim().to_ascii_lowercase()];
                names.extend(f[1].split_whitespace().map(|a| a.to_ascii_lowercase()));
                for name in names {
                    stmt.execute(params![name, f[2].trim(), f[3].trim(), f[4].trim(), f[5].trim()])?;
                    dirs += 1;
                }
            }
        }
        dtx.commit()?;
    }

    println!("imported {entries} instructions ({rows} rows) + {dirs} directive rows into {db}");
    Ok(())
}
