# Structured programming with `proc`

In WRASM the **`proc`** is the unit of structured programming. It is not just a
label with a `ret` — it is a *declared subroutine* with four properties the
assembler understands, every one of which lowers to **visible** instructions
(inspect any of it with `--emit-asm`; nothing is generated behind your back):

1. **A subroutine** — `proc NAME … endproc`, with a visible prologue/epilogue.
2. **A checked contract** — `uses` / `in` / `out` / `frame`, validated by `--check`.
3. **Structured control flow** — `.if` / `.while` / `.for` / … in the body, each
   lowering to a plain `cmp` + branch.
4. **A private label scope** — labels inside the proc are private to it, so its
   jump targets never collide with another proc's.

Together they give you the structured-programming discipline — single entry, no
jumping into the middle, scoped names, a declared interface — on top of raw x86-64.

## 1. The subroutine

```asm
proc DrawSpan  uses rbx rsi rdi  in rcx rdx r8  frame
    ; rcx = x, rdx = y, r8d = len   (the declared inputs)
    …
endproc
```

`proc` emits the label plus a prologue that pushes each `uses` register (and, with
`frame`, aligns the stack and reserves shadow space *once*). `endproc` — and any
`ret` / `.ret` inside the body — emits the matching epilogue, so a return can never
skip the restore. The exact frame layout is in
[macros.md → proc / endproc](macros.md#proc--endproc).

## 2. The contract — declare it, and it's checked

| clause | declares |
|---|---|
| `uses r…` | callee-saved registers the body clobbers — push/pop emitted, **and checked** |
| `in r…` | input registers — must be read before they're overwritten |
| `out r…` | result register — must be written in the body |
| `frame` | establish an aligned frame + 32-byte shadow space; **required if the body has `invoke`/`call`** |

`was prog.was --check` flags a callee-saved register clobbered without a `uses`, an
`in` register read after it's been destroyed, a promised `out` that's never written,
and a frame imbalance. The declaration *is* the subroutine's interface, and the
checker holds you to it. (It tracks GP registers only — `xmm6`–`xmm15` are your
responsibility; see `help.md` trap #2.)

## 3. Structured control flow — blocks, not hand-rolled labels

Inside a proc you write structured blocks; each lowers to a visible compare and
branch, so the listing reads the way you wrote it:

```asm
proc CountKeys  in rcx  out eax        ; rcx = 256-byte key table → eax = # held
    xor   eax, eax
    xor   edx, edx
    .while edx < KEY_COUNT             ; KEY_COUNT (=256) folds to a literal
        .if byte ptr [rcx + rdx] != 0
            inc eax
        .endif
        inc edx
    .endw
endproc
```

`.if/.elseif/.else/.endif`, `.while/.endw`, `.repeat/.until`,
`.for reg = a to b/.endfor`, `.forever`, and `.break`/`.continue`/`.ret` are all
available; a condition is `reg <relop> value` (`< <= > >= == !=`, unsigned by
default, `s`-prefix for signed). Full reference:
[macros.md → Structured control flow](macros.md#structured-control-flow-runtime).

## 4. The proc is a private label scope

The labels you *do* still write by hand — explicit `jmp`/branch targets — are
**private to the proc** (module scoping is on by default). So you needn't invent a
unique name for every loop in a multi-file module: each proc just uses plain
`loop:` / `done:` / `next:` and they can't collide.

```asm
module Render
proc DrawA
loop:                 ; this is DrawA$loop — private to DrawA
    …
    jmp loop
done:
    ret
endproc
proc DrawB
loop:                 ; this is DrawB$loop — a *different* label, no clash
    …
done:
    ret
endproc
endmodule
```

That is the structured-programming rule "**no GOTO into a subroutine**" made real:
a proc's internal labels are reachable only from inside it. Its *entry* — the
capitalised proc name `DrawA` — stays exported and callable; its *internals* are
not. (Capitalised and `.globl`'d names are always exported; build with the default
scoping, or `--nomodules` to revert to the old all-global labels — see
[help.md → Modules](../help.md).)

## Why it matters

Raw assembly has no subroutines, no scopes, no structured control flow — only bytes
and labels. `proc` layers all three on **as visible instructions plus a checker**,
so you get the safety of structured programming without giving up the transparency
of hand-written asm: the contract documents the interface, the control-flow blocks
keep the body readable, and the scope lets you stop worrying about label names. And
all of it is right there in `--emit-asm`.
