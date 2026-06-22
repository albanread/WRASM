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
        (Some("layout"), Some(name)) => match kb.layout(name)? {
            None => eprintln!("struct/union '{name}' not found"),
            Some(l) => {
                println!("{} {} : sizeof {}, align {}", l.kind, l.name, l.size, l.align);
                for f in &l.fields {
                    println!("  +{:<4} {:<28} {}", f.offset, f.name, f.type_name);
                }
            }
        },
        (Some("iface"), Some(name)) => match kb.interface(name)? {
            None => eprintln!("interface '{name}' not found"),
            Some(i) => {
                println!("interface {}", i.name);
                println!("  IID:  {}", i.iid.as_deref().unwrap_or("(none)"));
                println!("  base: {}", i.base.as_deref().unwrap_or("(none)"));
                for m in &i.methods {
                    println!("  vtbl[{:>2}] {}", m.vtable_index, m.name);
                }
                if let Some(m) = i.methods.first() {
                    println!("\n  ; in WRASM:  comcall pObj, {}, {}, <args…>", i.name, m.name);
                    println!("  ;            iid {}    ; emits the 16 IID bytes", i.name);
                }
            }
        },
        // A ready-to-paste struct-instance skeleton (leaf fields, dotted paths).
        (Some("skel"), Some(name)) => match kb.layout(name)? {
            None => eprintln!("struct/union '{name}' not found"),
            Some(l) => {
                println!("inst struct {}        ; {} bytes — fill in, drop unused lines", l.name, l.size);
                for f in &l.fields {
                    // Strip an array `[N]`/`[]` or pointer `*` decoration before the
                    // layout lookup — `short()` only drops the namespace, so without
                    // this an array-of-struct field (e.g. RenderTarget[]) never resolves
                    // and falls through to a bare leaf.
                    let is_array = f.type_name.contains('[');
                    let bare = match f.type_name.find('[') {
                        Some(i) => &f.type_name[..i],
                        None => f.type_name.as_str(),
                    };
                    let base = short(bare.trim_end_matches('*').trim());
                    match kb.layout(base)? {
                        // A genuine nested struct/union (or an array of one): expand its
                        // leaves. An array expands element [0] -> `field[0].sub`; the rest
                        // follow at +sizeof each. Single-field handle/BOOL wrappers stay one leaf.
                        Some(s) if s.fields.len() > 1 => {
                            let head = if is_array { format!("{}[0]", f.name) } else { f.name.clone() };
                            for sf in &s.fields {
                                let path = format!("{}.{}", head, sf.name);
                                println!("    {:<28} = 0   ; {}", path, sf.type_name);
                            }
                            if is_array {
                                let elem = kb.sizeof(base)?.unwrap_or(0);
                                println!("    ; ^ {}[0]; further elements at +{} bytes each", f.name, elem);
                            }
                        }
                        _ => println!("    {:<28} = 0   ; {}", f.name, f.type_name),
                    }
                }
                println!("ends");
            }
        },
        (Some("snippet"), Some(name)) => match kb.snippet(name)? {
            None => eprintln!("function '{name}' not found"),
            Some(s) => println!("{s}"),
        },
        (Some("suggest"), Some(name)) => {
            for s in kb.suggest(name, 5)? {
                println!("{s}");
            }
        }
        (Some("complete"), Some(prefix)) => {
            let scope = args.get(3).map(String::as_str).unwrap_or("all");
            let hits = kb.complete(prefix, scope, 30)?;
            for c in &hits {
                println!("{:<10} {:<40} {}", c.kind, c.name, c.detail);
            }
            eprintln!("\n{} candidate(s) [{scope}]", hits.len());
        }
        _ => {
            eprintln!(
                "usage:\n  \
                 winkb search <fragment>\n  \
                 winkb show <function>\n  \
                 winkb resolve <name>\n  \
                 winkb layout <struct>\n  \
                 winkb iface <interface>\n  \
                 winkb skel <struct>         struct-instance skeleton to paste"
            );
        }
    }
    Ok(())
}

/// Last component of a possibly fully-qualified type name.
fn short(ty: &str) -> &str {
    ty.rsplit('.').next().unwrap_or(ty)
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
