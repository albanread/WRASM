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
    // `Struct.field` → the struct's layout card (the dual of `Interface::Method`):
    // field access like `WNDCLASSEXW.cbSize` resolves to its containing struct, so
    // the offset table — including that field — is shown.
    if let Some((stru, _field)) = q.split_once('.') {
        if let Some(md) = struct_card(kb, stru.trim())? {
            return Ok(md);
        }
    }
    // Cards that need no db — the ABI and the ISA. A register or mnemonic is an
    // unambiguous keyword in asm, so resolve it ahead of the db lookups.
    if let Some(md) = register_card(q) {
        return Ok(md);
    }
    if let Some(md) = mnemonic_card(q) {
        return Ok(md);
    }
    // `was` language constructs (proc/frame/…) — no db, just the convention.
    if let Some(md) = was_card(q) {
        return Ok(md);
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

/// A card for a symbol defined in THIS buffer rather than in winkb: a local
/// code label/function, a data variable, or a `struct` instance. Pure source
/// scan, no db — `src` is the editor buffer. `None` if `name` isn't defined here.
pub fn local_card(src: &str, name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let is_data_type = |w: &str| {
        matches!(
            w.to_ascii_lowercase().as_str(),
            "byte" | "sbyte" | "word" | "sword" | "dword" | "sdword" | "qword" | "sqword"
                | "wchar" | "char" | "real4" | "real8" | "tbyte"
        )
    };
    let exported = src.lines().any(|l| {
        let t = l.trim();
        t.strip_prefix(".globl")
            .or_else(|| t.strip_prefix(".global"))
            .is_some_and(|r| r.trim() == name)
    });
    let mut in_code = true;
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        match line.to_ascii_lowercase().as_str() {
            ".code" | ".text" => in_code = true,
            ".data" => in_code = false,
            _ => {}
        }
        let Some(rest) = line.strip_prefix(name) else { continue };
        // Require a token boundary so `render` doesn't match `rendered:`.
        if rest.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let after = rest.trim_start();
        let lineno = i + 1;
        let decl = raw.trim_end();
        // `name:` — a label.
        if after.starts_with(':') {
            let (kind, hint) = if in_code {
                ("local code label", format!("A `call {name}` / `jmp {name}` here jumps to your own code."))
            } else {
                ("local data label", format!("Reference it PC-relative: `[rip + {name}]`."))
            };
            return Some(local_md(name, kind, exported, lineno, decl, &hint));
        }
        let first = after.split_whitespace().next().unwrap_or("");
        // `name struct TYPE …` — a struct instance laid out at db offsets.
        if first.eq_ignore_ascii_case("struct") {
            let ty = after.split_whitespace().nth(1).unwrap_or("?");
            let kind = format!("local `{ty}` instance");
            let hint = format!("Address it `[rip + {name}]`; fields resolve via the db (`[reg + {ty}.field]`).");
            return Some(local_md(name, &kind, exported, lineno, decl, &hint));
        }
        // `name <TYPE> …` — a data variable.
        if is_data_type(first) {
            let kind = format!("local data (`{}`)", first.to_ascii_uppercase());
            let hint = format!("Reference it PC-relative: `[rip + {name}]`.");
            return Some(local_md(name, &kind, exported, lineno, decl, &hint));
        }
    }
    None
}

fn local_md(name: &str, kind: &str, exported: bool, lineno: usize, decl: &str, hint: &str) -> String {
    let exp = if exported { "  ·  exported (`.globl`)" } else { "" };
    format!(
        "# {name}  —  {kind}{exp}\n\nDefined at line {lineno} of this file:\n\n```was\n{}\n```\n\n{hint}\n\n*Your own symbol — winkb has no entry for it.*\n",
        decl.trim_start()
    )
}

/// Documentation for a `was` language construct (currently the `proc`/`frame`
/// subroutine convention). Reached by a caret on the keyword or a search for it.
/// `None` if `name` isn't a documented construct.
pub fn was_card(name: &str) -> Option<String> {
    let n = name.trim().trim_start_matches('.').to_ascii_lowercase();
    if !matches!(
        n.as_str(),
        "proc" | "endproc" | "frame" | "uses" | "ret" | "return" | "subroutine" | "prologue" | "epilogue"
    ) {
        return None;
    }
    Some(
        "# proc … endproc  —  a checked subroutine\n\n\
A declared subroutine: the prologue and epilogue are generated as **visible** \
`push`/`pop`, and the body is checked against the contract. Nothing hidden.\n\n\
```was\n\
proc NAME  uses rbx rsi  in rcx rdx  out rax  frame\n\
    … body …\n\
endproc\n\
```\n\n\
| clause | meaning |\n\
|---|---|\n\
| `uses R…` | Callee-saved registers the body clobbers — pushed in the prologue, popped at `endproc`/`ret`. The contract check **errors** if the body writes any *other* callee-saved register (you'd silently destroy the caller's value). |\n\
| `in R…` | Input registers — documentation (drives the uninitialized-input check). |\n\
| `out R…` | Result registers — documentation (drives the unset-output check). |\n\
| `frame` | Reserve the 32-byte shadow space + outgoing-arg area **once** in the prologue (a single visible `sub rsp, K`, 16-aligned). Each `invoke`/`comcall` inside then drops its per-call alignment — just the arg moves and the `call`. |\n\n\
- A bare `ret` inside a proc restores the saved registers first; **`.ret`** is the explicit early exit.\n\
- A `proc` inside a `proc` is an error.\n\
- Dual check, caller side: the **clobber** warning flags a value left in a volatile register (rcx/rdx/r8–r11) across a call that destroys it.\n\n\
*Generated, but never hidden — the `push`/`pop` and the one-time `sub rsp, K` are real instructions in your listing and byte view.*\n"
            .to_string(),
    )
}

/// The register / ABI card: a register's conventional role and — the thing you
/// actually need mid-line — whether an `invoke`/`call` destroys it. Pure static
/// Win64 knowledge, no db. `None` if `name` isn't a register.
pub fn register_card(name: &str) -> Option<String> {
    let canon = canon_reg(name)?;
    let (role, volatile, notes) = reg_info(&canon);
    let mut s = format!("# {canon}  —  {role}\n\n");
    if volatile {
        s.push_str(
            "**Volatile** (caller-saved) — an `invoke`/`call` may destroy it; save it \
             yourself if you need the value afterward.\n",
        );
    } else {
        s.push_str(
            "**Preserved** (callee-saved) — it survives an `invoke`/`call`. If you clobber \
             it in your own code, `push`/`pop` it (restore before `ret`).\n",
        );
    }
    if !notes.is_empty() {
        s.push('\n');
        for n in &notes {
            s.push_str(&format!("- {n}\n"));
        }
    }
    s.push_str(
        "\n*Win64 call ABI:* integer args in **RCX, RDX, R8, R9** (5th+ on the stack); \
         float args in **XMM0–3**; return in **RAX** / **XMM0**. Caller reserves a **32-byte \
         shadow space**; **RSP is 16-byte aligned at every `call`**.\n",
    );
    Some(s)
}

/// Where `invoke` puts the n-th argument (1-based) under the Win64 ABI: the
/// first four integer/pointer args in rcx/rdx/r8/r9 (float args in xmm0–3 by
/// position), the rest on the stack above the 32-byte shadow space.
fn abi_slot(n: u32, type_name: &str) -> String {
    let t = type_name.to_ascii_lowercase();
    let is_float = t.contains("float") || t.contains("double");
    match n {
        1..=4 => {
            let i = (n - 1) as usize;
            if is_float {
                format!("`xmm{i}`")
            } else {
                ["`rcx`", "`rdx`", "`r8`", "`r9`"][i].to_string()
            }
        }
        _ => format!("`[rsp+{}]`", 32 + 8 * (n - 5)),
    }
}

/// The instruction card — the mnemonics that trip people up: the signed/unsigned
/// conditional-jump split, the implicit `rdx:rax` of mul/div, the flag-only
/// `cmp`/`test`, the `cl`-count shifts. Pure static ISA knowledge, no db. `None`
/// for mnemonics not worth a reminder.
pub fn mnemonic_card(name: &str) -> Option<String> {
    let m = name.trim().to_ascii_lowercase();
    if let Some(card) = jcc_card(&m) {
        return Some(card);
    }
    let (title, body): (&str, &str) = match m.as_str() {
        "cmp" => ("compare — subtract, set flags, discard the result",
            "Sets ZF/SF/CF/OF from `dst - src` but stores nothing. Follow with a conditional jump — **signed** `jg/jge/jl/jle` or **unsigned** `ja/jae/jb/jbe`. Picking the wrong family is the classic bug."),
        "test" => ("bitwise AND for flags — discard the result",
            "Sets ZF/SF/PF from `dst & src`, clears CF/OF, stores nothing. `test rax, rax` then `jz`/`js` is the idiom for is-zero / is-negative."),
        "mul" => ("unsigned multiply — implicit rdx:rax",
            "`mul r/m` does rax × r/m into **rdx:rax** (high half in rdx). Use `imul` for signed."),
        "imul" => ("signed multiply",
            "The 2/3-operand forms (`imul dst, src`) write one register; the 1-operand form uses **rdx:rax** like `mul`."),
        "div" => ("unsigned divide — implicit rdx:rax",
            "Divides **rdx:rax** by r/m → quotient in rax, remainder in rdx. **Zero rdx first** (`xor edx, edx`), or a stale rdx faults (#DE)."),
        "idiv" => ("signed divide — implicit rdx:rax",
            "Divides **rdx:rax** by r/m. **Sign-extend rax into rdx first** with `cdq`/`cqo` — not `xor edx, edx`."),
        "cdq" | "cqo" | "cwd" | "cbw" => ("sign-extend the accumulator",
            "`cdq` fills edx, `cqo` fills rdx, from the sign of the accumulator — the required setup before `idiv`."),
        "lea" => ("load effective address — no memory access",
            "Stores the address `[…]` would form; touches no memory and **sets no flags**. Doubles as cheap arithmetic: `lea rax, [rax + rax*4]`."),
        "shl" | "shr" | "sal" | "sar" => ("shift — variable count from cl only",
            "A register count must be **cl** (or an imm8). `shl`/`shr` are logical; `sar` is arithmetic (keeps the sign); `sal` == `shl`."),
        "rol" | "ror" | "rcl" | "rcr" => ("rotate — count from cl or imm8",
            "`rcl`/`rcr` rotate through CF; `rol`/`ror` don't."),
        "movzx" => ("move zero-extended",
            "Widens an 8/16-bit source into a 32/64-bit dest, zero-filling the top. (A 32-bit `mov` already zeroes the top 32 bits.)"),
        "movsx" | "movsxd" => ("move sign-extended",
            "Widens a smaller source into the dest, copying the sign bit through the top."),
        "movabs" => ("move a 64-bit immediate",
            "The only `mov` form that loads a full 64-bit immediate into a register."),
        "add" | "sub" | "and" | "or" | "xor" | "adc" | "sbb" | "neg" => ("ALU op — sets flags",
            "Writes the destination and sets ZF/SF/CF/OF — so a following `jcc` reads *this* result. `xor reg, reg` is the idiom for a zeroed register."),
        "inc" | "dec" => ("increment / decrement — sets flags except CF",
            "Updates ZF/SF/OF but **leaves CF unchanged** (unlike `add`/`sub` by 1) — a gotcha if a later branch expects CF."),
        "push" | "pop" => ("stack push / pop",
            "Moves rsp by 8 (in 64-bit) and transfers the operand. Keep the net effect even so rsp stays 16-byte aligned at calls."),
        "call" => ("call — push return address, jump",
            "Pushes the return address (rsp -= 8) then jumps. The callee expects **rsp 16-byte aligned at the `call`** and **32 bytes of shadow space** already reserved."),
        "ret" => ("return — pop into rip",
            "Pops the return address into rip. The stack must be exactly where the matching `call` left it."),
        _ => return None,
    };
    Some(format!("# {m}  —  {title}\n\n{body}\n"))
}

/// Conditional-jump card: meaning, **signedness**, and the flags it tests.
fn jcc_card(m: &str) -> Option<String> {
    let (meaning, kind, flags, also): (&str, &str, &str, &str) = match m {
        "je" | "jz" => ("equal / zero", "", "ZF = 1", "jne/jnz"),
        "jne" | "jnz" => ("not equal / not zero", "", "ZF = 0", "je/jz"),
        "jg" | "jnle" => ("greater", "signed", "ZF = 0 and SF = OF", "unsigned: `ja`"),
        "jge" | "jnl" => ("greater or equal", "signed", "SF = OF", "unsigned: `jae`"),
        "jl" | "jnge" => ("less", "signed", "SF ≠ OF", "unsigned: `jb`"),
        "jle" | "jng" => ("less or equal", "signed", "ZF = 1 or SF ≠ OF", "unsigned: `jbe`"),
        "ja" | "jnbe" => ("above", "unsigned", "CF = 0 and ZF = 0", "signed: `jg`"),
        "jae" | "jnb" | "jnc" => ("above or equal / no carry", "unsigned", "CF = 0", "signed: `jge`"),
        "jb" | "jnae" | "jc" => ("below / carry", "unsigned", "CF = 1", "signed: `jl`"),
        "jbe" | "jna" => ("below or equal", "unsigned", "CF = 1 or ZF = 1", "signed: `jle`"),
        "js" => ("sign (negative)", "", "SF = 1", "jns"),
        "jns" => ("not sign (non-negative)", "", "SF = 0", "js"),
        "jo" => ("overflow", "", "OF = 1", "jno"),
        "jno" => ("no overflow", "", "OF = 0", "jo"),
        "jp" | "jpe" => ("parity even", "", "PF = 1", "jnp/jpo"),
        "jnp" | "jpo" => ("parity odd", "", "PF = 0", "jp/jpe"),
        "jrcxz" => ("rcx is zero", "", "rcx == 0 (tests the register, not a flag)", ""),
        _ => return None,
    };
    let kindline = match kind {
        "signed" => "  ·  **signed**",
        "unsigned" => "  ·  **unsigned**",
        _ => "",
    };
    let note = match kind {
        "signed" => "\n\n⚠ **Signed** test — using it to compare *unsigned* values is a classic bug (addresses, sizes, counts are unsigned).",
        "unsigned" => "\n\n⚠ **Unsigned** test — for *signed* values use the signed sibling.",
        _ => "",
    };
    let also = if also.is_empty() { String::new() } else { format!("\n\nSibling: {also}.") };
    Some(format!(
        "# {m}  —  jump if {meaning}{kindline}\n\nTaken when **{flags}**. Set the flags with `cmp`/`test`/an ALU op on the line(s) just above.{note}{also}\n"
    ))
}

/// Canonical 64-bit name for any width of a register (`eax`→`rax`, `r8d`→`r8`);
/// `xmm/ymm/zmm` fold to `xmmN`. `None` if `name` isn't a register.
fn canon_reg(name: &str) -> Option<String> {
    let n = name.trim().to_ascii_lowercase();
    let gp = match n.as_str() {
        "rax" | "eax" | "ax" | "al" | "ah" => "rax",
        "rbx" | "ebx" | "bx" | "bl" | "bh" => "rbx",
        "rcx" | "ecx" | "cx" | "cl" | "ch" => "rcx",
        "rdx" | "edx" | "dx" | "dl" | "dh" => "rdx",
        "rsi" | "esi" | "si" | "sil" => "rsi",
        "rdi" | "edi" | "di" | "dil" => "rdi",
        "rbp" | "ebp" | "bp" | "bpl" => "rbp",
        "rsp" | "esp" | "sp" | "spl" => "rsp",
        "rip" => "rip",
        _ => "",
    };
    if !gp.is_empty() {
        return Some(gp.to_string());
    }
    for base in 8..=15u8 {
        if [format!("r{base}"), format!("r{base}d"), format!("r{base}w"), format!("r{base}b")]
            .contains(&n)
        {
            return Some(format!("r{base}"));
        }
    }
    for pfx in ["xmm", "ymm", "zmm"] {
        if let Some(num) = n.strip_prefix(pfx) {
            if let Ok(k) = num.parse::<u8>() {
                if k < 16 {
                    return Some(format!("xmm{k}"));
                }
            }
        }
    }
    None
}

/// Role, volatility (true = caller-saved/destroyed by a call), and notes.
fn reg_info(canon: &str) -> (String, bool, Vec<String>) {
    let s = |x: &str| x.to_string();
    match canon {
        "rax" => (s("return value"), true, vec![
            s("Holds the call's return value — HRESULT, handle, count, pointer."),
            s("Implicitly read/written by `mul`, `imul`, `div`, `idiv`, `cdq`/`cqo`."),
        ]),
        "rcx" => (s("integer argument 1"), true, vec![
            s("First integer/pointer argument."),
            s("COM vtable calls pass `this` here (argument 0): `mov rax,[rcx]; call [rax + slot*8]`."),
            s("Shift/rotate counts come from `cl`."),
        ]),
        "rdx" => (s("integer argument 2"), true, vec![
            s("Second integer/pointer argument."),
            s("Upper half of the dividend (`rdx:rax`) in `div`/`idiv` — set it with `cdq`/`cqo`."),
        ]),
        "r8" => (s("integer argument 3"), true, vec![s("Third integer/pointer argument.")]),
        "r9" => (s("integer argument 4"), true, vec![
            s("Fourth integer/pointer argument."),
            s("The 5th argument onward goes on the stack, above the 32-byte shadow space."),
        ]),
        "r10" | "r11" => (s("scratch"), true, vec![s("Caller-saved scratch — no argument role.")]),
        "rbx" | "r12" | "r13" | "r14" | "r15" => (s("preserved general"), false, vec![
            s("Callee-saved: a value here survives an `invoke`. If you clobber it, `push`/`pop` it."),
        ]),
        "rbp" => (s("frame pointer (by convention)"), false, vec![
            s("Callee-saved. Conventionally the frame pointer, but usable as a general register if you save it."),
        ]),
        "rsi" | "rdi" => (s("preserved general"), false, vec![
            s("Callee-saved **on Windows x64** — survives a call."),
            s("Gotcha: on System V (Linux/macOS) RSI/RDI are *argument* registers and volatile — Win64 differs."),
            s("Implicit source/dest of the string ops (`movs`, `stos`, `lods`)."),
        ]),
        "rsp" => (s("stack pointer"), false, vec![
            s("Keep **16-byte aligned at every `call`** (so the callee sees `rsp % 16 == 8` after the return-address push)."),
            s("Reserve **32 bytes of shadow space** below the four register arguments before calling."),
        ]),
        "rip" => (s("instruction pointer"), false, vec![
            s("Not directly writable; `[rip + sym]` is how data is addressed PC-relative."),
        ]),
        c if c.starts_with("xmm") => {
            let k: u8 = c[3..].parse().unwrap_or(0);
            match k {
                0 => (s("float argument 1 / float return"), true, vec![
                    s("First floating-point argument; also the float/double return value."),
                ]),
                1..=3 => (format!("float argument {}", k + 1), true, vec![
                    s("XMM0–3 carry the first four floating-point arguments."),
                ]),
                4 | 5 => (s("scratch"), true, vec![s("Caller-saved scratch.")]),
                _ => (s("preserved"), false, vec![
                    s("XMM6–15 are callee-saved (their low 128 bits) — a value here survives a call."),
                ]),
            }
        }
        _ => (s("register"), true, vec![]),
    }
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
        s.push_str("| # | parameter | type | slot | values |\n|--:|---|---|---|---|\n");
        for (i, p) in f.params.iter().enumerate() {
            s.push_str(&format!(
                "| {} | `{}` | {} | {} | {} |\n",
                p.ordinal,
                p.name,
                short_type(&p.type_name),
                abi_slot((i + 1) as u32, &p.type_name),
                values_cell(&p.related),
            ));
        }
        s.push('\n');
        s.push_str(
            "*Slots: integer/pointer args → rcx, rdx, r8, r9, then the stack above the \
             32-byte shadow space; float args → xmm0–3. This is what `invoke` marshals.*\n\n",
        );
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
        assert!(md.contains("| # | parameter | type | slot | values |"), "table:\n{md}");
        assert!(md.contains("```was"), "insert frame:\n{md}");
        assert!(md.contains("invoke CreateFileW"), "invoke:\n{md}");
        // dwShareMode is an enum param: its members should populate a value cell.
        assert!(md.contains("FILE_SHARE"), "enum values:\n{md}");
        // The marshaling column: arg 1 → rcx, and a stack slot for later args.
        assert!(md.contains("`rcx`"), "arg1 slot:\n{md}");
        assert!(md.contains("[rsp+"), "stack slot for later args:\n{md}");
    }

    #[test]
    fn mnemonic_card_flags_the_signed_unsigned_split() {
        let jg = mnemonic_card("jg").unwrap();
        assert!(jg.contains("signed") && jg.contains("ZF = 0 and SF = OF"), "jg:\n{jg}");
        let jb = mnemonic_card("jb").unwrap();
        assert!(jb.contains("unsigned") && jb.contains("CF = 1"), "jb:\n{jb}");
        assert!(mnemonic_card("idiv").unwrap().contains("rdx:rax"), "idiv implicit ops");
        assert!(mnemonic_card("inc").unwrap().contains("CF"), "inc/dec CF gotcha");
        assert!(mnemonic_card("lea").unwrap().contains("no flags"));
        assert!(mnemonic_card("frobnicate").is_none());
    }

    #[test]
    fn abi_slot_follows_win64() {
        assert_eq!(abi_slot(1, "HANDLE"), "`rcx`");
        assert_eq!(abi_slot(4, "DWORD"), "`r9`");
        assert_eq!(abi_slot(5, "LPVOID"), "`[rsp+32]`");
        assert_eq!(abi_slot(6, "int"), "`[rsp+40]`");
        assert_eq!(abi_slot(1, "float"), "`xmm0`"); // float args go in xmm by position
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
    fn proc_doc_card_from_keyword_and_search() {
        let md = was_card("proc").expect("proc");
        assert!(md.contains("checked subroutine"), "title:\n{md}");
        assert!(md.contains("`uses`") && md.contains("`frame`") && md.contains("contract check"));
        // reachable from the related keywords (and a leading-dot directive) + a search term
        for k in ["frame", "endproc", "uses", ".ret", "subroutine"] {
            assert!(was_card(k).is_some(), "{k} should resolve");
        }
        assert!(was_card("mov").is_none());
        // routes through answer() too — the search-box path
        let Some(kb) = kb() else { return };
        assert_eq!(answer(&kb, "proc").unwrap(), md);
    }

    #[test]
    fn register_card_states_role_and_volatility() {
        // Pure Win64 ABI — no db. Any width folds to the 64-bit name.
        let rcx = register_card("ecx").expect("ecx → rcx");
        assert!(rcx.contains("# rcx") && rcx.contains("argument 1"), "role:\n{rcx}");
        assert!(rcx.contains("Volatile") && rcx.contains("this"), "volatility + COM this:\n{rcx}");
        let rsi = register_card("rsi").expect("rsi");
        assert!(rsi.contains("Preserved") && rsi.contains("Windows"), "rsi callee-saved on Win64:\n{rsi}");
        assert!(register_card("xmm3").unwrap().contains("argument 4"), "xmm numbering");
        assert!(register_card("r12").unwrap().contains("Preserved"));
        assert!(register_card("notareg").is_none());
    }

    #[test]
    fn local_card_recognizes_own_symbols() {
        // Pure source scan — no db.
        let src = ".DATA\ncbData DWORD 0,0,0,0\n.CODE\n.globl main\nmain:\n    ret\nrender:\n    ret\n";
        let r = local_card(src, "render").expect("render");
        assert!(r.contains("# render") && r.contains("local code label"), "kind:\n{r}");
        assert!(r.contains("line 7"), "line number:\n{r}");
        assert!(local_card(src, "main").unwrap().contains("exported"), "main is .globl");
        assert!(local_card(src, "cbData").unwrap().contains("local data (`DWORD`)"), "data type");
        assert!(local_card(src, "render").unwrap() != local_card(src, "main").unwrap());
        // `render` must match as a whole token, not a prefix.
        assert!(local_card("rendered:\n", "render").is_none(), "no prefix match");
        assert!(local_card(src, "nope").is_none());
        assert!(local_card(src, "CreateFileW").is_none(), "winkb names aren't local");

        let s = ".DATA\nscd struct DXGI_SWAP_CHAIN_DESC\n    BufferCount = 1\nends\n";
        assert!(local_card(s, "scd").unwrap().contains("DXGI_SWAP_CHAIN_DESC"), "struct instance");
    }

    #[test]
    fn struct_field_access_resolves_to_the_struct_card() {
        // The dual of `Interface::Method`: `Struct.field` (e.g. the `WNDCLASSEXW.cbSize`
        // a `mov [rdi + WNDCLASSEXW.cbSize]` uses) answers with the struct's layout.
        let Some(kb) = kb() else { return };
        let md = answer(&kb, "WNDCLASSEXW.cbSize").unwrap();
        assert_eq!(md, answer(&kb, "WNDCLASSEXW").unwrap(), "same as the bare struct");
        assert!(md.contains("# WNDCLASSEXW") && md.contains("`cbSize`"), "struct card:\n{md}");
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
