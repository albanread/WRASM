# rust-tcl

`rust-tcl` is a small, fast Tool Command Language core for Rust applications.
It is Tcl/Tk-inspired, but designed for application-owned verbs: the language
kernel parses and runs scripts, while each embedding project registers the verbs
that make sense for its own automation surface.

The embedded language core is complete for the Tcl-flavoured baseline:
commands, substitution, control flow, procedures, `uplevel`/`upvar`, lists, and
dictionaries.

## Pipeline

Scripts run through explicit stages:

1. Lexer
2. Parser
3. Semantic analysis
4. CFG construction
5. Bytecode generation
6. Bytecode VM

Known native command names are resolved during semantic analysis to stable
`VerbId`s, so those calls dispatch by ID. Tcl-style procedures and dynamic
command names compile to runtime command lookup.

## Core Syntax

- Commands are separated by newlines or `;`.
- Words are whitespace-separated.
- Braced words, `{like this}`, are literal.
- Quoted and bare words support `$name`, `${name}`, backslash escapes, and
  command substitution with `[command args]`.
- Control flow and procedures use familiar Tcl forms: `if`, `while`,
  `foreach`, `break`, `continue`, `proc`, and `return`.
- Tcl scope features `uplevel` and `upvar` are included.
- Lists and dictionaries use Tcl string representations.

## Core Verbs

The default registry currently includes:

- `set name ?value?`
- `append name value...`
- `puts value`
- `expr`
- `if`, `while`, `foreach`, `break`, `continue`
- `proc`, `return`
- `uplevel`, `upvar`
- `list`, `llength`, `lindex`, `lrange`, `lappend`
- `dict`
- `add`, `sub`, `mul`, `div`
- `eq`
- `concat`
- `error`

Applications extend the language by registering verbs:

```rust
use rust_tcl::{Arity, Registry, Value};

let mut registry = Registry::with_core();
registry.register("tool/status", Arity::exact(1), |_, args| {
    Ok(Value::new(format!("checked {}", args[0])))
});
```

## CLI

```powershell
cargo run -- run --result -e 'set x [add 20 22]; puts "answer=$x"; set x'
```
