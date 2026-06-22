# WRASM — the dialect, and where it bites

WRASM is the assembly dialect of the **`was`** front-end: **Intel-syntax x86-64 for
Windows**, with **MASM-style macros** (`invoke` / `proc` / `struct` / `sizeof`) and a
knowledge database that resolves Windows API signatures, named constants, struct field
offsets, and `sizeof`. If you know Intel/MASM you already know most of it — so this page
is the **differences and the traps**, not a full reference.

Deep references: [instruction set](docs/instructions.md) · [registers & addressing](docs/language.md)
· [macros & contracts](docs/macros.md) · the framework that exercises it all:
[gamecanvas](docs/gamecanvas.md), [gameaudio](docs/gameaudio.md).

## CLI

```
was <input.was> -o <out.exe|out.obj>   assemble (.exe = self-contained PE, else COFF)
was <input.was> --emit-asm             print the lowered rasm text, then stop
was <input.was> --check                semantic check only (diagnostics, no output)
was <input.was> -o x.exe --entry NAME  set the PE entry symbol (default: main)
was --help                             a condensed version of this page
```

The knowledge DB (`$WINKB_DB`, else `E:\windows_api\windows_api.db`) is what lets
`invoke User32!MessageBoxW`, a bare constant like `MB_OK`, `STRUCT.field`, and
`sizeof(STRUCT)` resolve with no manual declarations.

## At a glance (coming from Intel/MASM)

- **Intel syntax**: `mov dst, src`; memory `[rip + label]`, `[reg + index*scale + disp]`,
  `[rip + label + disp]`. Size overrides `byte ptr` / `dword ptr` / etc.
- **Sections** `.DATA` / `.CODE`; labels `name:`; export with `.globl name`.
- **Data directives**: `BYTE WORD DWORD QWORD WCHAR`, and **`real4`/`real8`** (which take
  *decimal float literals* — `freq real8 440.0` — and emit IEEE-754 bits). `N dup(v)`
  repeats, `?` zero-fills, `"..."` is a string (`WCHAR` → UTF-16).
- **Macros**: `invoke`, `proc`/`endproc`, `struct`/`ends`, `comcall`, `sizeof`,
  `.include "file"` (path relative to the including file).
- **Transparency is the whole point**: every macro expands to *visible* instructions —
  nothing is generated behind your back. `was x.was --emit-asm` prints exactly the bytes'
  worth of instructions you're getting. If a macro hides an instruction, that's a bug.

## The `proc` contract system

```
proc NAME uses <nonvol regs> in <arg regs> out <reg> frame
    …body…
endproc
```

- **`uses`** — the non-volatile GP registers the proc may clobber (it saves/restores
  them): `rbx rsi rdi rbp r12 r13 r14 r15`.
- **`in`** — the GP argument registers it reads (checked: read before you clobber them).
- **`out`** — the result register (documentation + checking).
- **`frame`** — establish an aligned stack frame + 32-byte shadow space. **Required for
  any proc that contains `invoke` or `call`.**
- The checker (run by `--check`, and live in the IDE) flags: clobbering a non-volatile
  not listed in `uses`, reading an `in` register after it's been overwritten, and frame
  imbalance. It is **GP-register only** — see trap #2.

## Exceptions & traps — where WRASM bites

These are the dialect's sharp edges. Most were found the hard way; learn them once.

1. **`invoke` uses `rax`/`eax` as scratch.** To stage stack arguments it emits
   `mov rax, <stackarg>; mov [rsp+N], rax` *before* loading the register args — so a
   value you computed into `rax`/`eax` and then pass to `invoke` gets clobbered.
   **Never pass an `invoke` argument that lives in `rax`/`eax`**; route it through a
   memory slot or another register. *(Symptom: an argument silently arrives as 0.)*

2. **xmm volatility, and the contract doesn't track it.** Win64: **`xmm0`–`xmm5` are
   volatile, `xmm6`–`xmm15` are callee-saved.** The `uses` checker tracks **GP registers
   only** — it will *not* catch a clobbered `xmm6+`. So a `proc` may use `xmm0`–`xmm5`
   as free scratch, but must save `xmm6`–`xmm15` itself if it touches them; and a caller
   may keep state in `xmm6`–`xmm15` across a `call` only if the callee honors that.

3. **`frame` is required for `invoke`/`call`.** Without it there's no aligned shadow
   space and `--check` rejects the proc. Leaf procs that call nothing don't need it.

4. **Float args to `invoke` need an annotation.** Tag them `real4`/`real8` so they
   marshal to an xmm register: `invoke f, real8 [rip + x]`.

5. **`dup` count must be a literal.** `BYTE 64 dup(0)` works; `BYTE 8*8 dup(0)` does not
   (no arithmetic folding in the count).

6. **No manual `sub rsp` inside a `frame` proc** ("rsp off the frame level"). To keep a
   value across a `call`, use `xmm0`–`xmm5` (if the callee honors xmm6+) or a `.DATA`
   slot — the latter is single-threaded only.

7. **A `','` char literal trips the lexer** ("malformed char literal"). Use the ASCII
   number instead (`44`). An apostrophe literal (`0x27`) is fine.

## See also

- [docs/macros.md](docs/macros.md) — `invoke` lowering, the `proc` frame layout, the
  contract checker in detail.
- [docs/language.md](docs/language.md) — registers, addressing modes, size overrides.
- [docs/instructions.md](docs/instructions.md) — the supported instruction set.
- [docs/gamecanvas.md](docs/gamecanvas.md) / [docs/gameaudio.md](docs/gameaudio.md) —
  the 2D-game framework written entirely in WRASM (the largest worked example).
