//! ide — the assembler's assistant, as content.
//!
//! Every panel in the IDE is one winkb query rendered a particular way. Rather
//! than hand-build card widgets, we turn a query into **markdown** and let
//! `docpane` (the Direct2D markdown/DirectWrite engine, shared with WF66) draw
//! it. `doc_help` stays for regular help; this is the live API assistant.
//!
//! This module is deliberately GUI-free so the *content* — the hard, knowledge-
//! driven half — is unit-testable in the terminal before any pixels exist:
//!
//!   ide::answer(&kb, "CreateFileW")  -> the function card (markdown)
//!   ide::answer(&kb, "RECT")         -> the struct layout card
//!   ide::answer(&kb, "IShellItem")   -> the interface / vtable card
//!   ide::answer(&kb, "file")         -> a search result list
//!
//! ## Interactive widgets (forward-compatible)
//!
//! A function card's insert frame is the centerpiece: the user fills the holes
//! and double-clicks to drop a correct `invoke` into the editor. We emit two
//! placeholder forms that the (to-be-extended) docpane parser will render as
//! real controls, and that read fine as plain text until then:
//!
//!   {{field:NAME}}              -> a text input, NAME as its placeholder
//!   {{select:NAME|A,B,C}}       -> a dropdown; first option is the default
//!
//! `insert_frame()` produces a line in this form; `function_card()` embeds the
//! plain (already-valid) snippet plus a parameter table whose value column is
//! the dropdown source. Extending docpane upgrades those in place — the
//! markdown is the single source of truth either way.

use anyhow::Result;
use winkb::{Func, Kb};

pub mod widget;

/// How many enum members to list inline before eliding the rest.
const MAX_VALUES_INLINE: usize = 12;

/// Answer a free-form query the way the assistant pane will: resolve it to the
/// most specific card we can (function → struct → interface), else a search
/// result list. Returns markdown.
pub fn answer(kb: &Kb, query: &str) -> Result<String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok("# Search\n\nType a function, type, or fragment.\n".to_string());
    }
    // `Interface::Method` → a COM method card.
    if let Some((iface, method)) = q.split_once("::") {
        if let Some(md) = method_card(kb, iface.trim(), method.trim())? {
            return Ok(md);
        }
    }
    if let Some(md) = function_card(kb, q)? {
        return Ok(md);
    }
    if let Some(md) = struct_card(kb, q)? {
        return Ok(md);
    }
    if let Some(md) = interface_card(kb, q)? {
        return Ok(md);
    }
    search_card(kb, q)
}

/// The function card: signature, docs link, a parameter table whose value
/// column lists the valid constants per enum param (the dropdown source), and a
/// ready-to-insert `invoke` frame. `None` if `name` isn't a known function.
pub fn function_card(kb: &Kb, name: &str) -> Result<Option<String>> {
    let Some(f) = kb.function(name)? else { return Ok(None) };
    let mut s = String::new();

    let dll = f.dll.as_deref().unwrap_or("?");
    let cc = f.callconv.as_deref().unwrap_or("");
    s.push_str(&format!("# {}\n\n", f.name));
    s.push_str(&format!(
        "`{}` **{}**({} param{}) · {}{}{}\n\n",
        f.ret,
        f.name,
        f.params.len(),
        if f.params.len() == 1 { "" } else { "s" },
        dll,
        if cc.is_empty() { String::new() } else { format!(" · {cc}") },
        f.aw_family
            .as_deref()
            .map(|a| format!(" · {a} family"))
            .unwrap_or_default(),
    ));
    if let Some(url) = &f.doc_url {
        s.push_str(&format!("[Documentation]({url})\n\n", url = url));
    }

    if !f.params.is_empty() {
        s.push_str("| # | parameter | type | values |\n|--:|---|---|---|\n");
        for p in &f.params {
            s.push_str(&format!(
                "| {} | `{}` | {} | {} |\n",
                p.ordinal,
                p.name,
                short_type(&p.type_name),
                values_cell(&p.related),
            ));
        }
        s.push('\n');
    }

    s.push_str("### Insert\n\n");
    s.push_str("```was\n");
    // The plain frame is already valid asm (enum params defaulted to a real
    // member, others `<field>`), so it renders/copies usefully even before the
    // docpane widget extension lands.
    if let Some(snip) = kb.snippet(name)? {
        s.push_str(snip.trim_end());
        s.push('\n');
    }
    s.push_str("```\n");

    Ok(Some(s))
}

/// The interactive insert line: the form the extended docpane renders as fields
/// and dropdowns. `{{select:..}}` for enum params (members as options),
/// `{{field:..}}` otherwise. `None` if `name` isn't a known function.
pub fn insert_frame(kb: &Kb, name: &str) -> Result<Option<String>> {
    let Some(f) = kb.function(name)? else { return Ok(None) };
    let mut args = Vec::with_capacity(f.params.len());
    for p in &f.params {
        if p.related.is_empty() {
            args.push(format!("{{{{field:{}}}}}", p.name));
        } else {
            let opts = p
                .related
                .iter()
                .map(|(m, _)| m.as_str())
                .collect::<Vec<_>>()
                .join(",");
            args.push(format!("{{{{select:{}|{}}}}}", p.name, opts));
        }
    }
    Ok(Some(if args.is_empty() {
        format!("invoke {}", f.name)
    } else {
        format!("invoke {}, {}", f.name, args.join(", "))
    }))
}

/// The struct/union card: size, alignment, and field byte offsets.
pub fn struct_card(kb: &Kb, name: &str) -> Result<Option<String>> {
    let Some(l) = kb.layout(name)? else { return Ok(None) };
    let mut s = String::new();
    s.push_str(&format!("# {}  ({})\n\n", l.name, l.kind));
    s.push_str(&format!("sizeof **{}** · align **{}**\n\n", l.size, l.align));
    if !l.fields.is_empty() {
        s.push_str("| offset | field | type |\n|--:|---|---|\n");
        for fld in &l.fields {
            s.push_str(&format!("| +{} | `{}` | {} |\n", fld.offset, fld.name, short_type(&fld.type_name)));
        }
        s.push('\n');
    }
    s.push_str("### Insert\n\n```was\n");
    let var = l.name.to_ascii_lowercase();
    s.push_str(&format!("; reserve one {}\n{var}: .zero {}\n", l.name, l.size));
    if let Some(first) = l.fields.first() {
        // Field access uses the `Struct.field` idiom was resolves to a byte
        // offset — verified to lower inside a memory operand.
        s.push_str(&format!(
            "\n; read a field (was resolves {}.{} to its byte offset)\nmov  eax, [rcx + {}.{}]    ; +{}\n",
            l.name, first.name, l.name, first.name, first.offset,
        ));
    }
    s.push_str("```\n");
    Ok(Some(s))
}

/// The COM interface card: IID, base, and methods in absolute vtable order.
pub fn interface_card(kb: &Kb, name: &str) -> Result<Option<String>> {
    let Some(i) = kb.interface(name)? else { return Ok(None) };
    let mut s = String::new();
    s.push_str(&format!("# {}  (interface)\n\n", i.name));
    s.push_str(&format!(
        "IID `{}`{}\n\n",
        i.iid.as_deref().unwrap_or("(none)"),
        i.base.as_deref().map(|b| format!(" · base `{b}`")).unwrap_or_default(),
    ));
    if !i.methods.is_empty() {
        s.push_str("| vtbl | method |\n|--:|---|\n");
        for m in &i.methods {
            // Link each method to its own card (`Interface::Method`).
            s.push_str(&format!("| {} | [`{}`](was:{}::{}) |\n", m.vtable_index, m.name, i.name, m.name));
        }
        s.push('\n');
    }
    Ok(Some(s))
}

/// A concise card for a COM method `Interface::Method`: which interface, which
/// vtable slot (walking the base chain for inherited methods), and the two ways
/// to call it in WRASM. `None` if the interface or method isn't known.
pub fn method_card(kb: &Kb, interface: &str, method: &str) -> Result<Option<String>> {
    if kb.interface(interface)?.is_none() {
        return Ok(None);
    }
    // Find the absolute vtable slot and which interface in the chain owns it.
    let mut name = interface.to_string();
    let mut found: Option<(i64, String)> = None;
    for _ in 0..32 {
        let Some(iface) = kb.interface(&name)? else { break };
        if let Some(m) = iface.methods.iter().find(|m| m.name == method) {
            found = Some((m.vtable_index, iface.name.clone()));
            break;
        }
        match iface.base {
            Some(b) => name = b.rsplit('.').next().unwrap_or(&b).to_string(),
            None => break,
        }
    }
    let Some((slot, owner)) = found else { return Ok(None) };

    let mut s = format!("# {interface}::{method}  (COM method)\n\n");
    s.push_str(&format!("Vtable slot **{slot}** of [`{interface}`](was:{interface})"));
    if owner != interface {
        s.push_str(&format!(" · inherited from `{owner}`"));
    }
    s.push_str(".\n\n### Call it\n\n```was\n");
    s.push_str(&format!("p.{method}(args…)\n"));
    s.push_str(&format!("comcall p, {interface}, {method}, args…\n"));
    s.push_str("```\n\n");
    s.push_str(&format!("The `p.{method}(…)` form needs `comobj p : {interface}`.\n"));
    Ok(Some(s))
}

/// A search result list: matches as in-pane navigation links. The `was:` scheme
/// is what the pane intercepts to load that item's card.
pub fn search_card(kb: &Kb, query: &str) -> Result<String> {
    let hits = kb.search(query, 40)?;
    let mut s = format!("# Results for “{query}”\n\n");
    if hits.is_empty() {
        s.push_str("_No matches._");
        for alt in kb.suggest(query, 5)? {
            s.push_str(&format!("\n\nDid you mean [`{alt}`](was:{alt})?"));
            break;
        }
        return Ok(s);
    }
    for h in &hits {
        let detail = if h.detail.is_empty() { String::new() } else { format!(" — {}", h.detail) };
        s.push_str(&format!("- [`{}`](was:{}) · _{}_{}\n", h.name, h.name, h.kind, detail));
    }
    s.push_str(&format!("\n_{} result(s)._", hits.len()));
    Ok(s)
}

/// Render an enum param's members as a value cell, eliding a long tail.
fn values_cell(related: &[(String, u64)]) -> String {
    if related.is_empty() {
        return "—".to_string();
    }
    let shown = related
        .iter()
        .take(MAX_VALUES_INLINE)
        .map(|(m, _)| format!("`{m}`"))
        .collect::<Vec<_>>()
        .join(" · ");
    if related.len() > MAX_VALUES_INLINE {
        format!("{shown} … (+{})", related.len() - MAX_VALUES_INLINE)
    } else {
        shown
    }
}

/// Trim a fully-qualified Windows-metadata type to its last component, so a
/// table's type column shows `BITMAPINFO*` instead of the whole namespace path
/// `Windows.Win32.Graphics.Gdi.BITMAPINFO*`. Short names pass through unchanged.
fn short_type(t: &str) -> &str {
    t.rsplit('.').next().unwrap_or(t)
}

#[allow(dead_code)]
fn signature_line(f: &Func) -> String {
    let params = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.type_name, p.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} {}({})", f.ret, f.name, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open the knowledge db, or skip the test if it isn't present in this env.
    fn kb() -> Option<Kb> {
        let path = std::env::var("WINKB_DB")
            .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
        Kb::open(&path).ok()
    }

    #[test]
    fn function_card_has_signature_values_and_frame() {
        let Some(kb) = kb() else { return };
        let md = function_card(&kb, "CreateFileW").unwrap().expect("CreateFileW");
        assert!(md.contains("# CreateFileW"), "title:\n{md}");
        assert!(md.contains("KERNEL32"), "dll:\n{md}");
        assert!(md.contains("| # | parameter | type | values |"), "table:\n{md}");
        assert!(md.contains("```was"), "insert frame:\n{md}");
        assert!(md.contains("invoke CreateFileW"), "invoke:\n{md}");
        // dwShareMode is an enum param: its members should populate a value cell.
        assert!(md.contains("FILE_SHARE"), "enum values:\n{md}");
    }

    #[test]
    fn insert_frame_uses_widgets() {
        let Some(kb) = kb() else { return };
        let line = insert_frame(&kb, "CreateFileW").unwrap().expect("CreateFileW");
        assert!(line.starts_with("invoke CreateFileW, "), "{line}");
        assert!(line.contains("{{field:"), "has a text field: {line}");
        assert!(line.contains("{{select:") && line.contains("FILE_SHARE"), "has a dropdown: {line}");
    }

    #[test]
    fn struct_card_has_offsets() {
        let Some(kb) = kb() else { return };
        let md = struct_card(&kb, "RECT").unwrap().expect("RECT");
        assert!(md.contains("sizeof **16**"), "size:\n{md}");
        assert!(md.contains("`left`") && md.contains("`right`"), "fields:\n{md}");
        assert!(md.contains("| offset | field | type |"), "table:\n{md}");
    }

    #[test]
    fn interface_card_has_iid_and_vtable() {
        let Some(kb) = kb() else { return };
        let md = interface_card(&kb, "IShellItem").unwrap().expect("IShellItem");
        assert!(md.contains("43826d1e"), "iid:\n{md}");
        assert!(md.contains("| vtbl | method |"), "vtable:\n{md}");
    }

    #[test]
    fn insert_frame_round_trips_through_widget_model() {
        let Some(kb) = kb() else { return };
        let line = insert_frame(&kb, "CreateFileW").unwrap().expect("CreateFileW");
        let spans = widget::parse(&line);
        // Every parameter becomes exactly one interactive hole, in order.
        let f = kb.function("CreateFileW").unwrap().unwrap();
        assert_eq!(widget::holes(&spans).len(), f.params.len());
        // Defaulting the holes yields a valid invoke line (dropdowns → a real
        // constant, fields → <placeholder>).
        let defaulted = widget::defaults(&spans);
        assert!(defaulted.starts_with("invoke CreateFileW, "));
        assert!(defaulted.contains("FILE_SHARE_NONE"));
        assert!(defaulted.contains("<lpFileName>"));
    }

    #[test]
    fn answer_dispatches_and_search_links() {
        let Some(kb) = kb() else { return };
        assert!(answer(&kb, "CreateFileW").unwrap().contains("# CreateFileW"));
        assert!(answer(&kb, "RECT").unwrap().contains("sizeof"));
        let results = answer(&kb, "CreateFile").unwrap();
        assert!(results.contains("(was:"), "nav links:\n{results}");
    }
}
