# rust-tcl Language Guide

`rust-tcl` is a Tcl-flavoured embedded language for Rust tools. The goal is
familiar Tcl syntax with a compiled implementation:

```text
source -> lexer -> parser -> sema -> cfg -> bytecode -> vm
```

The core language owns syntax, substitution, control flow, procedures, Tcl-style
scope commands, lists, dictionaries, bytecode, and execution. Each embedding
application registers its own domain verbs.

The language core is now considered complete for the embedded Tcl-flavoured
baseline. Further growth should usually happen as application verbs.

## Scripts And Commands

A script is a sequence of commands separated by newlines or `;`.

```tcl
set x 40
set y [expr {$x + 2}]
puts "answer=$y"
```

Commands use Tcl's familiar shape: command name first, then words. Known native
verbs compile to direct `VerbId` calls. Procedure and dynamic command calls
compile to runtime command lookup, preserving Tcl's command model.

```tcl
set cmd puts
$cmd "hello"
```

## Comments

A `#` starts a comment at the beginning of a command, after optional whitespace.

```tcl
# comment
set x 1
```

## Words

Words are separated by whitespace unless grouped.

Bare and quoted words support variable substitution, command substitution, and
backslash escapes:

```tcl
set name Ada
puts "hello $name"
puts answer=[expr {20 + 22}]
```

Braced words are literal:

```tcl
puts {$name [expr {20 + 22}]}
```

That prints:

```text
$name [expr {20 + 22}]
```

## Substitution

Variable substitution:

```tcl
set x 42
puts $x
puts ${x}
```

Command substitution:

```tcl
set x [expr {20 + 22}]
puts "answer=$x"
```

Nested commands are compiled into bytecode. The VM does not re-parse source to
run command substitutions.

## Values

All language values are strings, following Tcl. Structured values such as lists
and dictionaries use Tcl-style canonical string representations.

```tcl
set xs [list alpha {two words} omega]
lindex $xs 1
```

The result of a script is the result of its final command.

## Expressions

`expr` is a compiled language form for conditions and arithmetic.

```tcl
expr {$x + 1}
expr {$x < 10 && $x != 3}
```

Supported expression operators:

```text
!  - unary
*  /
+  -
<  <=  >  >=
==  !=
&&  ||
```

## Control Flow

`if` compiles to conditional bytecode jumps:

```tcl
if {$x < 10} {
    puts small
} elseif {$x == 10} {
    puts exact
} else {
    puts large
}
```

`while` compiles to a loop:

```tcl
set x 0
while {$x < 5} {
    set x [expr {$x + 1}]
}
```

`foreach` iterates Tcl list values with bytecode loop frames:

```tcl
set out {}
foreach x [list 1 2 3 4] {
    if {$x == 2} {continue}
    if {$x == 4} {break}
    lappend out $x
}
```

`break` and `continue` are compiled control-flow operations inside loops.

## Procedures

`proc` defines a compiled user command:

```tcl
proc add2 {x {y 2}} {
    return [expr {$x + $y}]
}

add2 40
```

Procedure arguments use Tcl list syntax. Defaults are written as nested list
elements, as in Tcl: `{name default}`.

`return` exits the current procedure and returns a value:

```tcl
return "done"
```

## Scope Commands

Tcl's `uplevel` and `upvar` are unusual, but expected by people who know Tcl.
They are part of the core.

`uplevel` runs a script in another stack frame. With no explicit level, it runs
one level up:

```tcl
set x original

proc change {} {
    uplevel {set x changed}
}

change
set x
```

`uplevel` also accepts a level:

```tcl
uplevel 0 {set x current}
uplevel #0 {set globalValue yes}
```

Multiple script words are joined with spaces, matching Tcl's familiar command
shape:

```tcl
uplevel set x changed
```

`upvar` aliases a variable in another frame into the current frame:

```tcl
proc bump {varName} {
    upvar $varName v
    set v [expr {$v + 1}]
}

set n 41
bump n
set n
```

`upvar` supports explicit levels and multiple alias pairs:

```tcl
upvar #0 globalName localName
upvar 1 left x right y
```

## Lists

Lists are strings with Tcl list quoting. Core list verbs include:

```tcl
list value...
llength list
lindex list index
lrange list first last
lappend varName value...
```

Examples:

```tcl
set xs [list alpha {two words} omega]
llength $xs       # 3
lindex $xs 1      # two words
lrange $xs 0 end  # alpha {two words} omega
```

## Dictionaries

Dictionaries use the Tcl key/value list representation.

```tcl
set d [dict create name Ada role engineer]
dict set d role lead
dict get $d role
```

Core dictionary subcommands:

```tcl
dict create key value...
dict get dictionary ?key?
dict exists dictionary key
dict set varName key value
dict keys dictionary
dict values dictionary
```

## Core Verbs

The default registry includes:

```text
set, append, puts
expr
if, while, foreach, break, continue
proc, return
uplevel, upvar
list, llength, lindex, lrange, lappend
dict
add, sub, mul, div, eq, concat, error
```

Some of these are language forms compiled directly by codegen (`if`, `while`,
`foreach`, `proc`, `return`, `break`, `continue`, `expr`). Scope-sensitive
forms (`uplevel`, `upvar`) are core control verbs backed by VM stack-frame
operations. Others are normal native verbs registered in the core registry.

## Application Verbs

Applications extend the language by registering verbs:

```rust
use rust_tcl::{Arity, Registry, Value};

let mut registry = Registry::with_core();

registry.register("locus/check", Arity::exact(1), |_, args| {
    Ok(Value::new(format!("checked {}", args[0])))
});
```

Scripts can then call the verb:

```tcl
locus/check src/main.locus
```

Use namespaced command names for application verbs:

```text
locus/check
locus/effects
docrate/index
docrate/query
```

## Bytecode Model

The bytecode instruction set includes stack operations, native calls, dynamic
calls, procedure definition, jumps, expression evaluation, foreach frames, and
procedure return.

Representative instructions:

```text
PushConst id
LoadVar id
EvalExpr id
Concat count
Call verb_id argc
CallName name_id argc
CallDynamic argc
DefineProc name proc
JumpIfFalse target
Jump target
ForeachStart var end
ForeachNext body end
ForeachPop
Return
Pop
Halt
```

Known native application verbs take the fast `Call VerbId` path. Tcl-style
procedures and dynamic command names use `CallName` or `CallDynamic`.

## CLI

Run an inline script:

```powershell
cargo run -- run --result -e 'set x [expr {20 + 22}]; puts "answer=$x"; set x'
```

Dump bytecode:

```powershell
cargo run -- run --dump-bytecode --result -e 'set x [expr {20 + 22}]; set x'
```

Read a script file:

```powershell
cargo run -- run script.tcl
```

## Current Boundaries

This is a complete small language core, not Tcl's full standard library. It does
not include namespaces, channels/files, packages, async/event-loop integration,
or Tk. Those should arrive only when embedding applications need them, and most
new behavior should be application verbs rather than core syntax.
