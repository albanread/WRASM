//! ide-card — print the markdown the assistant pane would render for a query.
//!
//!   ide-card CreateFileW        # function card
//!   ide-card RECT               # struct layout card
//!   ide-card IShellItem         # interface / vtable card
//!   ide-card file               # search results
//!   ide-card --frame CreateFileW   # just the interactive insert line
//!
//! DB path: $WINKB_DB, else E:\windows_api\windows_api.db.

use std::process::ExitCode;

use winkb::Kb;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let db = std::env::var("WINKB_DB")
        .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
    let kb = Kb::open(&db)?;

    let (frame_only, query): (bool, Option<&str>) = match args.get(1).map(String::as_str) {
        Some("--frame") => (true, args.get(2).map(String::as_str)),
        other => (false, other),
    };

    let Some(query) = query else {
        eprintln!("usage: ide-card [--frame] <query>");
        return Ok(());
    };

    if frame_only {
        match ide::insert_frame(&kb, query)? {
            Some(line) => println!("{line}"),
            None => eprintln!("function '{query}' not found"),
        }
    } else {
        print!("{}", ide::answer(&kb, query)?);
    }
    Ok(())
}
