//! winkb — CLI over the Windows API knowledge base. The pane in a terminal.
//!
//!   winkb search <fragment>     list matching functions / constants / types
//!   winkb show <function>       signature + the constants/types each param uses
//!   winkb resolve <name>        a constant/enum name -> its value
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

    let cmd = args.get(1).map(String::as_str);
    let arg = args.get(2).map(String::as_str);

    let kb = Kb::open(&db)?;
    match (cmd, arg) {
        (Some("search"), Some(frag)) => {
            let hits = kb.search(frag, 40)?;
            for h in &hits {
                println!("{:<10} {:<44} {}", h.kind, h.name, h.detail);
            }
            eprintln!("\n{} result(s)", hits.len());
        }
        (Some("resolve"), Some(name)) => {
            let vals = kb.resolve(name)?;
            if vals.is_empty() {
                eprintln!("'{name}' not found in constants or enum members");
            }
            for v in &vals {
                let ns = v.namespace.as_deref().unwrap_or("");
                println!(
                    "{name} = {} (0x{:x})  [{}]  {ns}",
                    v.i64v, v.bits, v.source
                );
            }
        }
        (Some("show"), Some(name)) => match kb.function(name)? {
            None => eprintln!("function '{name}' not found"),
            Some(f) => print_func(&f),
        },
        _ => {
            eprintln!(
                "usage:\n  \
                 winkb search <fragment>\n  \
                 winkb show <function>\n  \
                 winkb resolve <name>"
            );
        }
    }
    Ok(())
}

fn print_func(f: &winkb::Func) {
    let dll = f.dll.as_deref().unwrap_or("?");
    let cc = f.callconv.as_deref().unwrap_or("?");
    let aw = f.aw_family.as_deref().map(|a| format!(" [{a}]")).unwrap_or_default();
    println!("{} {}({} args)  {dll}  {cc}{aw}", f.ret, f.name, f.params.len());
    if let Some(url) = &f.doc_url {
        println!("  docs: {url}");
    }
    for p in &f.params {
        println!(
            "  {:>2}  {:<24} {} [{}]",
            p.ordinal, p.name, p.type_name, p.type_kind
        );
        for (m, v) in &p.related {
            println!("        {m} = 0x{v:x}");
        }
    }
}
