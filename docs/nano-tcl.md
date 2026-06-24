# nano-TCL — design & plan

An in-app **register-machine REPL**, hand-written in x86-64 and embedded in the game
`.exe` as a library module (`library/nanotcl/`). It is *not* a toolchain feature and
*not* studio's full TCL — it is a tiny tool command language whose only purpose is
**in-app dev automation** and **per-global-proc testing**: type `Pset rcx=10, rdx=20,
r8=14`, hit a frame, see the pixel. Companion docs: [studio-tcl.md](studio-tcl.md) (the
IDE's TCL), [structured.md](structured.md), [gamecanvas.md](gamecanvas.md).

Like everything in WRASM, it is **transparency-first**: every byte the interpreter
executes is ordinary hand-written asm visible in `--emit-asm`, and the one generated
artefact — the verb table — *prints what it emits*. No hidden codegen, no signature
reflection, no runtime assembler.

## The two tiers

nano-TCL is a **two-tier split interpreter**, and the split is the whole design:

- **Tier 1 (Part 1, below)** — a *dumb-but-real* register machine, hand-written in asm
  and embedded in the game `.exe`. No variables, no `if`, no loops — it only executes
  `ProcName reg=val,…` lines against the live process and captures the register file.
- **Tier 2 (Part 2)** — a *smart* `rust_tcl`-based driver on the developer's machine that
  runs **full TCL** (variables, `expr`, `if`/`while`/`foreach`/`proc`) locally and
  forwards only a final, fully-substituted `ProcName reg=val` line over a channel.

The two are **two 60 fps loops** trading one message round per frame — async on the wire
(id-correlated REQUEST / REPLY / EVENT, §13), frame-locked in cadence:

```
  developer's machine — Tier 2 (Rust / rust_tcl)        the game .exe — Tier 1 (asm)
 ┌──────────────────────────────────────────┐          ┌────────────────────────────┐
 │ full TCL: set expr if while foreach proc  │  REQUEST │ nano-TCL executor (Part 1)  │
 │           $vars   — all LOCAL             │ ════════▶│  parse CALL / REG / STEP    │
 │ · eval thread   runs script, blocks/call  │ id CALL… │  nanoCall load·call·capture │
 │ · demux thread  id-routes reply/event     │◀════════ │  shadow register file       │
 │ · 60 fps display  live dashboard          │ REPLY /  │  the REAL Pset/Beep/… procs │
 └──────────────────────────────────────────┘ EVENT    └────────────────────────────┘
   smart half = Rust: vars, formatting,        ── one round per frame ──  dumb-but-real
   the 60 fps display                                                     half = asm
```

Why split this way? Variables, control flow, expression evaluation, and *all numeric
formatting and display* are hard in asm and free in `rust_tcl` — so they live entirely in
the Rust tool and never touch the wire. The game returns **raw register bytes**; the tool
formats and renders them. Tier 1 stays the tiny, transparent register machine; the wire
stays minimal. (Tier 1 can also run **standalone** — a baked-in script with no tool
attached — which is the only mode that needs the in-`.exe` numeric printing of §5.)

## Status

| Area | State | Where |
|---|---|---|
| Shadow register file (`.data` image) | ⬜ design | `nanotcl/shadow.was` |
| Register-name lexer | ⬜ design | `nanotcl/lexreg.was` |
| Load–call–capture trampoline | ⬜ design | `nanotcl/call.was` |
| Verb dispatch table + `was --emit-verbs` | ⬜ design | `nanotcl/verbs.was` |
| Value lexer (int / hex / float / str / reg) | ⬜ design | `nanotcl/lexval.was` |
| Parse loop + `set`/`puts`/`expect`/`step` | ⬜ design | `nanotcl/repl.was` |
| Numerics via Win32 (standalone `puts` only — §5) | ⬜ design | `nanotcl/num.was` |
| Live-injection control channel | ⬜ design | `nanotcl/inject.was` |
| Tier-2 `rust_tcl` driver + dashboard (Part 2) | ⬜ design | `crates/nanotcl/` |

## 1. Purpose & the model

A game made of dozens of exported procs (`Pset`, `DrawSprite`, `Beep`, `ParseTune`, …)
needs a way to **poke each one in isolation** — at runtime, in the real process, with
the real palette and the real audio device — without recompiling a throwaway harness.
nano-TCL is that poke.

The model is a **register machine**. Its *entire* state is one **shadow register
file**: a `.data` image with a slot per CPU register (`rax rbx rcx rdx rsi rdi r8..r15`,
`xmm0..xmm15`, a flags slot). **The variable namespace *is* the register names.** There
are no user variables, no symbol table, no scoping — `rcx` is a variable, `xmm0` is a
variable, and after a call `eax` holds the return value.

Why a register machine, and why **explicit `reg=value`** instead of positional args?
Because it deletes the two hardest problems at a stroke:

- **It kills the signature table.** A positional caller (`Pset 10 20 14`) must know that
  arg 1 → `ecx`, arg 2 → `edx`, arg 3 → `r8b` *for every proc* — a per-proc signature
  database the interpreter would have to carry and keep in sync. With `Pset rcx=10,
  rdx=20, r8=14` the **script names the register**, so dispatch needs nothing but a
  *name → address* table. No introspection. No reflection. No signatures.
- **It kills the float wall.** Positional dispatch can't tell "put this in a GP reg"
  from "put this in an XMM reg" without type info per slot. Here the register name *is*
  the type: `xmm0=1.4` is unambiguously a `double` in `xmm0`; `rcx=440` is an integer in
  `rcx`. The author writes the ABI, explicitly, every time — exactly the WRASM ethos.

The whole thing is honest about what it is: a thin, explicit, register-level call shim.
That honesty is the feature.

## 2. The language

Lexing reuses the library's existing char-by-char asm scanner
(`library/audio/music/abc_scan.was`): a global `txtPtr` cursor plus `SkipSpaces`,
`ParseUInt`, `SkipToEol`. nano-TCL adds a register-name lexer and a value lexer on top.

### Grammar (informal)

```
script   := statement (NEWLINE statement)*
statement:= verb-call | set | puts | expect | step | comment
verb-call:= NAME (reg '=' value (',' reg '=' value)*)?
set      := 'set' reg value
puts     := 'puts' expr
expect   := 'expect' expr expr
step     := 'step'                 ; run one game frame, then continue
comment  := '#' ... EOL
reg      := 'rax'|'eax'|...|'r15'|'xmm0'..'xmm15'   ; 32-bit aliases truncate
value    := int | hex | float | string | reg
```

### Value types

| form | example | lands in | how |
|---|---|---|---|
| integer | `100` | GP slot | `ParseUInt` (32-bit — see note) |
| hex / 64-bit | `0x40` / `4294967300` | GP slot | a new dec64/hex digit loop |
| float | `1.4` | XMM slot | `VarR8FromStr` (Win32, §5) |
| string | `"hi"` | GP slot | copy to an arg arena, store the **pointer** |
| register | `eax` | GP/XMM slot | copy the *source* slot → target slot (**chaining**) |

> **Note on integers.** `ParseUInt` (reused from `abc_scan.was`) returns a **32-bit**
> value in `eax` and does **not** handle `0x` — fine for the common game arg (a
> coordinate, an index, a frame count), which is what it's for. Values ≥ 2³² and hex
> literals go through a new ~20-line dec64/hex loop that writes the full 64-bit slot.
> A bare register on the right (`set rdx eax`) copies one slot to another — this is how
> you **chain** a return value into the next call.

### Verbs

- `Proc reg=val, …` — a capital-initial proc name: **load named regs → call →
  capture all regs** (§3). After it, every slot is a readable variable.
- `set reg val` — write a value into a slot, no call.
- `puts expr` — format and print a slot or literal (§5).
- `expect a b` — resolve both, compare; mismatch prints a diagnostic and sets the
  non-zero exit flag (a script doubles as a **test**).
- `step` — run exactly one `GameFrame` and return (§7) — advance the game between pokes.

### Worked example

```tcl
# poke the canvas, the audio, and a return value — then assert.

set rcx 10                  # variables are registers
Pset rcx=10, rdx=20, r8=14  # call Pset(ecx=x, edx=y, r8b=index)
step                        # advance one frame so the pixel presents

Beep rcx=440, rdx=120       # 440 Hz for 120 ms
Tone xmm0=1.4               # a proc taking a double — explicit XMM arg

ParseTune rcx=tuneStr       # returns note count in eax
puts eax                    # -> prints the captured return value
set rdx eax                 # CHAIN: copy eax into rdx for the next call
PlayN rdx=eax               # feed the captured count straight in

Add rcx=2, rdx=3            # a proc that returns rcx+rdx in eax
expect eax 5                # PASS — non-zero exit on mismatch
```

Every line is a register-level contract the reader can verify by hand.

## 3. The call ABI — load / call / capture

The trampoline is its **own `proc nanoCall frame`** — *that* is what guarantees the
16-byte stack alignment and the 32-byte shadow space the callee expects, regardless of
what the parser left on the stack. (Don't lean on the enclosing `GameFrame`'s frame: the
indirect `call [gVerbPtr]` happens several calls deep, so `nanoCall` must own its own
aligned frame.) The body is straight-line, **RIP-relative, and clobber-free**: it loads
**every** slot, calls, then captures **every** slot (a later optimisation builds a
per-call plan of just the slots the statement touched).

```asm
proc nanoCall frame                  ; <- its own frame: alignment + shadow space here
    lea   rbp, [rip + shadowGP]       ; rbp = base; callee-saved → survives the call
    ; --- load (rsp/rbp slots skipped: loading them corrupts the frame) ---
    mov   rax, [rbp+0]                ; rax
    mov   rcx, [rbp+8]                ; rcx
    mov   rdx, [rbp+16]               ; rdx
    ; … rbx rsi rdi r8..r15 …
    movsd xmm0, [rip + shadowXMM + 0]
    ; … xmm1..xmm15 …
    call  qword ptr [rip + gVerbPtr]  ; target address parked by dispatch (§4)
    ; --- capture: RFLAGS FIRST, then GP, then XMM ---
    pushfq                            ; grab the callee's flags before anything edits them
    mov   [rbp+0],  rax               ; save the return rax (mov doesn't touch flags)
    mov   [rbp+8],  rcx
    ; … rdx rbx rsi rdi r8..r15 …
    pop   rax                         ; rax already saved → reuse it for the flags
    mov   [rip + shadowFL], rax
    movsd [rip + shadowXMM + 0], xmm0
    ; … xmm1..xmm15 …
endproc
```

Key properties:

- **No GP scratch is clobbered between load and call.** The target is parked in a
  `.data` QWORD (`gVerbPtr`) *before* the load block, and the call is indirect through
  `[rip+gVerbPtr]`, so no register is needed to hold it. Every load is a literal
  `mov reg,[rbp+disp]` — nothing destroys the values just loaded.
- **`rbp` is the base and is callee-saved** by the dispatched proc, so it survives for
  the capture block. `rsp`/`rbp` slots exist in the file for completeness but are never
  loaded or stored.
- **Flags are captured first.** `pushfq` is the first op after the call so it reads the
  callee's `RFLAGS` (the GP/XMM stores that follow are `mov`/`movsd`, which don't touch
  flags — but `pushfq`-first makes that robust against future edits). The return `rax`
  is saved before `pop rax` reuses the register.

GP slot order is the standard ModRM encoding order (`rax=0, rcx=1, rdx=2, rbx=3, rsp=4,
rbp=5, rsi=6, rdi=7, r8..r15=8..15`) so a future encoder can reuse the layout:

```asm
    .balign 16
shadowGP   QWORD 16 dup(0)      ; +0..+120  GP, ModRM order
shadowFL   QWORD 0              ; +128      RFLAGS (pushfq)
    .balign 16
shadowXMM  OWORD 16 dup(0)      ; +144      xmm0..xmm15, 16 B each
    .balign 16
```

## 4. Dispatch — `name → address`, and `was --emit-verbs`

Dispatch is a flat table, walked linearly with a `strcmp` over the name:

```asm
verbTbl:
    QWORD vn_Pset,   Pset       ; { asciz-name-ptr, proc-address }
    QWORD vn_Beep,   Beep
    QWORD vn_Add,    Add
verbCount QWORD 3
```

Lookup: scan `verbTbl` for a matching `nameAsciz`, copy its `procAddr` into `gVerbPtr`,
call `nanoCall`. That's the whole dispatcher.

The table can be **generated**: `was --emit-verbs` walks the *procs* it parsed (so it
uses `parse_proc`'s names, **not** the raw label/`.globl` set — `HWndProc` is `.globl`'d
but is a Win32 callback, not a `reg=value` verb) and emits one `{ QWORD vn_Name, Name }`
row for each capital-initial **proc**, plus the count — **no signature/in-register
introspection**, because the script names the registers. Per the no-hidden-codegen rule,
`--emit-verbs` **prints every row to stdout as it writes it**, so the author sees exactly
what entered the table. For a first proof the table is hand-written; `--emit-verbs` just
produces the same text a human would.

> **Footgun (no type checking):** the table makes *every* capital-initial proc callable,
> including ones whose signature isn't `reg=value`-shaped (a Win32-callback-style proc,
> or one taking a struct pointer). They'll be *callable* but *nonsensical* — the
> interpreter trusts the author to call what makes sense (§8).

## 5. Numerics & printing via the Windows API — *standalone mode only*

> **When is this needed?** Only when a nano-TCL script runs **baked into the game with no
> Tier-2 tool attached** (§11) and so must print results itself. In the normal tool-driven
> path the game ships **raw register bytes** down the channel and the Rust tool does all
> formatting and display (§15) — so none of the Win32 machinery below is on that path.
> Treat this section as the optional offline `puts`.

The standing instruction is to **lean on the Windows API** rather than re-implement
conversion. All signatures below are verified via `winkb`. This is a CRT-free PE, so
anything that drags in `msvcrt` (`_i64toa`, `strtod`) is **out**.

| job | API | DLL | registers / notes |
|---|---|---|---|
| int64 → dec/hex string | `wsprintfA` | USER32 | **cdecl varargs**: `rcx`=buf, `rdx`=fmt (`"%I64d"`/`"%I64x"`), `r8`=value. Give a ≥32-byte buffer (no bounds check). Bounds-checked alt: `wnsprintfA` (SHLWAPI), length in `rdx`. |
| string → int64 | `ParseUInt` (asm) | — | already in `abc_scan.was` (32-bit, decimal). `StrToInt64ExA` (SHLWAPI: `rcx`=str, `rdx`=flags, `r8`=`LONGLONG*`) is the freebie for sign/`0x`/64-bit. |
| string → double | `VarR8FromStr` | OLEAUT32 | `rcx`=`LPCWSTR` (**UTF-16** → `MultiByteToWideChar(CP_ACP)` first), `rdx`=`lcid` (`LOCALE_INVARIANT 0x7F` so `.` is always the separator), `r8`=`dwFlags`(0), `r9`=`double*` (a `.data` qword). Returns `HRESULT` in `eax`; `S_OK`(0) = ok. |
| double → string | `VarBstrFromR8` | OLEAUT32 | `wsprintf has no float`. `xmm0`=`dblIn`, `rdx`=`lcid`(0x7F), `r8`=`dwFlags`(0), `r9`=`BSTR*`. Returns `HRESULT` in `eax`; allocates a **BSTR**. |
| output sink | `OutputDebugStringA` | KERNEL32 | one arg in `rcx`, no return; visible in DebugView / studio — the lowest-friction sink in a console-less GUI app. |

Three lifecycle facts to respect:

- **`wsprintfA` is cdecl varargs, but no args spill to the stack here** — `buf`/`fmt`/
  `value` all ride in `rcx`/`rdx`/`r8`, so the cdecl-vs-stdcall *stack-cleanup* question
  never bites (there's nothing on the stack to clean). Just don't let `invoke` assume a
  return convention it shouldn't.
- **A `double` first arg goes in `xmm0`** (`VarBstrFromR8`), with `rcx` *homed/skipped*
  per the Win64 ABI; the integer args follow in `rdx`/`r8`/`r9` as listed above.
- **BSTR is UTF-16, length-prefixed, and must be freed.** `VarBstrFromR8` returns a
  `BSTR` whose 4-byte length sits *before* the char pointer; to print it,
  `WideCharToMultiByte` it to ASCII, then **`SysFreeString(bstr)`** (OLEAUT32) — every
  `puts xmm…` leaks otherwise.

For the **live-injection** path, `puts` is redirected from the debug sink into the channel
reply (§6): in standalone mode it formats locally (above); under a Tier-2 tool it need only
ship the **raw** value as an `out:`/EVENT line (§13–§15) and let the tool format it — so
even `puts` can skip the BSTR/`VarBstrFromR8` dance whenever a tool is attached.

## 6. Live injection — driving a running app

Sending TCL into the interpreter of an **already-running** app (from studio, a CLI) and
getting results back must feed a command into the single-threaded loop — a
`SetTimer`-driven `WM_TIMER` fires `GameFrame` ~60 fps via `GetMessageW`/
`DispatchMessageW` — **without stalling the 16 ms budget** and **without a blocking call
on the main thread**. Two correct designs; pick by how rich the reply needs to be.

### Simple & synchronous — `WM_COPYDATA`

The lowest-ceremony path, and the recommended first cut. The client does
`SendMessageW(hwnd, WM_COPYDATA, sender, &COPYDATASTRUCT{dwData, cbData=len, lpData=cmd})`;
the OS marshals the bytes into the game's address space and delivers `WM_COPYDATA`
(`0x4A`) to `HWndProc` **synchronously, on the message pump** (so it lands between
frames, with non-volatiles clean). Add `cmp edx, 0x4A; je hw_copydata` in `HWndProc`; the
handler reads `lpData`/`cbData` from the `COPYDATASTRUCT*` (in `r9`), points `txtPtr` at
it, runs the parse loop, and returns an `LRESULT`.

- **No pipe, no thread, no connection state** — the pump delivers it.
- **Reply is one integer** (`LRESULT`) — perfect for an `expect` pass/fail status. Rich
  `puts` text goes out-of-band to `OutputDebugStringA` (studio/DebugView captures it).
- Copy `lpData` before returning — it is only valid for the duration of the handler.

### Rich & bidirectional — a named-pipe reader thread

When you want `puts` text back **inline**, run a dedicated reader thread (the same
`CreateThread` + `lock xchg` spinlock pattern the audio mixer already uses in
`sound_play.was`) — **not** the deprecated `PIPE_NOWAIT` polled in the frame:

- **Reader thread:** `CreateNamedPipeA("\\.\pipe\nanotcl", PIPE_ACCESS_DUPLEX,
  PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE, 1, 4096, 4096, 0, 0)` (**blocking, message
  mode** so one `WriteFile` = one frame, §14), then a loop of `ConnectNamedPipe` (blocks
  until a client) → `ReadFile` (blocks for a command) → push the command into a small
  **spinlock-guarded queue** → repeat. The blocking I/O lives on this thread, so the frame
  loop never stalls and there's no `PIPE_NOWAIT`/per-frame `ConnectNamedPipe` state machine.
- **`GameFrame` hook (`TclPoll`):** drains the queue (spinlock — like the mixer reading
  its voice table), runs the parse loop for any queued command, and the main thread
  `WriteFile`s the captured response back on the duplex `hPipe`. **studio** is the natural
  client (it already speaks `rust_tcl`): open the pipe, `WriteFile` the script, `ReadFile`
  the reply.
- **Re-entrancy (real hazard).** `txtPtr` is a `.data` cursor, **not** a register, so the
  harness's register-shielding does **not** protect it: a queued script that runs `step`
  re-enters `GameFrame` → `TclPoll`, which would drain another command and clobber
  `txtPtr`. Guard with a `tclBusy` flag — `TclPoll` returns immediately while a script is
  running, so `step` advances a frame without re-draining. (§7.)

*(Winsock/TCP is possible — `socket`/`bind`/`listen` on `127.0.0.1`, the reader thread
`accept`/`recv` — and gives cross-machine reach, but it's heavier; reserved for a future
remote story.)*

### Security reality — dev-only, gated

An open local control channel **remotely drives the process**: it can `call` any
exported proc, so it is **arbitrary in-process execution**. Pipes default to a permissive
DACL — any local user could connect. The channel is therefore **dev-only**: gate the pipe
/ `WM_COPYDATA` handler / `TclPoll` behind a build flag (assemble only when
`NANOTCL_LIVE` is defined) or a runtime `--dev` switch. **Never ship it in a release
build.**

## 7. Game-loop integration

The REPL runs **at the frame boundary** — either inside `HWndProc`'s `WM_COPYDATA`
handler or via `TclPoll` at the top of `GameFrame` (`library/harness.was`). Two things to
be precise about:

- **A `Proc` call is safe for the cursor for free** — the called proc (`Pset`, `Beep`, …)
  is unrelated code that never touches nano-TCL's `txtPtr`, so the cursor is intact when
  the call returns. (`rbp`, the trampoline's base, survives because it's callee-saved.)
- **`step` needs a re-entry guard** — it is the *one* thing that re-enters the loop. Set
  `tclBusy=1` while a script is executing; `TclPoll` checks it and skips the drain, so
  `step`→`GameFrame`→`TclPoll` advances the frame **without** starting a second script and
  clobbering `txtPtr`. (Equivalently: save/restore `txtPtr` around any nested run.)

Because injection happens at the frame boundary, a multi-statement script with `step`
interleaves cleanly with the game's own update — the interpreter never preempts a
half-finished frame.

## 8. Limits & risks

- **`txtPtr` is shared, single-cursor state.** Re-entrancy via `step` is handled by the
  `tclBusy` guard (§7); without it, a nested drain corrupts the running script — this is
  the #1 thing to get right.
- **No SEH in the first proof.** A bad call (wrong reg, null pointer) faults the *whole
  process* — there's no try/catch around `call [gVerbPtr]` yet. Because you poke procs with
  *arbitrary* args, faults are **expected**, so the dev build should install a vectored
  exception handler (`AddVectoredExceptionHandler`) that captures the `EXCEPTION_RECORD` +
  `CONTEXT`, ships a fault dump down the channel as an **`exc` EVENT** (Part 2 §14), and
  returns `ERR` for that call so the session survives. That turns "the probe crashed the
  game" into "the probe reported a crash" — see Part 2 §15 (the exception channel).
- **Dev-only channel** — see §6 security; build-flag it out of release.
- **No type checking on the call** (§4) — `Pset xmm0=1.4` loads `xmm0` and calls `Pset`,
  which ignores it. The interpreter trusts the author's ABI knowledge — the point, and the
  footgun.
- **32-bit/decimal default** — bare integers use `ParseUInt` (32-bit); hex and ≥2³² need
  the dec64/hex loop (§2). `rsp`/`rbp` are not settable by design.
- **Single pipe instance** (`nMaxInstances=1`) — one dev client at a time.
- **Deferred:** per-call load plans (vs. load-everything), `loop N { }`, `if`, user-named
  aliases, a guarded/SEH call path, multi-client pipes.

## 9. Implementation phases

| phase | build | effort |
|---|---|---|
| **P0 — afternoon proof** | shadow file (§3) + register lexer + the `nanoCall` load-all/call/capture-all trampoline + a hand-written 2-verb table; drive it from a hard-coded in-`.data` string. Prove `Pset rcx=…, …` lights a pixel. | ~½ day |
| **P1 — scripts** | value lexer (int/hex/str/reg, **no float yet**) + the parse loop + `set`/`puts`(int)/`expect`/`step`. Run a `.tcl` string baked into the game; `puts` via `OutputDebugStringA`. | ~½ day |
| **P2 — the channel** | `WM_COPYDATA` handler first (synchronous, int reply); then the named-pipe reader thread + spinlock queue + `WriteFile` reply for rich `puts`. Gate behind `NANOTCL_LIVE`. | ~1 day |
| **P3 — `was --emit-verbs`** | the `was` flag that walks capital-initial **procs** and prints + emits `verbTbl`. Replaces the hand-written table. | ~½ day |
| **P4 — float conversions** | `VarR8FromStr` (parse `xmm0=1.4`) + `VarBstrFromR8` + `SysFreeString` (`puts xmm0`), with `MultiByteToWideChar`/`WideCharToMultiByte` + LCID `0x7F`. | ~½ day |

Rough total **≈ 460 LOC of asm** + the `--emit-verbs` flag in `was`.

## 10. File layout

```
library/nanotcl/
  nanotcl.was     ; thin shell of .includes (per the shell-of-includes convention)
  shadow.was      ; the shadow register-file .data image (§3)
  lexreg.was      ; register-name lexer → {kind, slot} (§2)
  lexval.was      ; value lexer: int / hex / float / string / reg (§2,§5)
  call.was        ; nanoCall load–call–capture trampoline + gVerbPtr (§3)
  verbs.was       ; verbTbl + lookup; target of --emit-verbs (§4)
  num.was         ; Win32 numeric parse/print wrappers (§5)
  repl.was        ; parse loop + set/puts/expect/step + tclBusy guard (§2,§7)
  inject.was      ; WM_COPYDATA + pipe reader thread + TclPoll + the exc VEH (§6,§15)
```

Reused, not duplicated: `library/audio/music/abc_scan.was` (`txtPtr`, `SkipSpaces`,
`ParseUInt`, `SkipToEol`) and the `CreateThread` + `lock xchg` spinlock pattern from
`library/audio/sound/sound_play.was` (the channel reader thread). Everything nano-TCL
adds is ordinary asm, visible in `--emit-asm`; the only generated artefact, `verbTbl`,
prints what it emits — a module like any other (`module Nanotcl … endmodule`), so its
internals are private and only its exported verbs/entry points are global.

---

# Part 2 — the developer-side tool (Tier 2)

Part 1 is **Tier 1**: a dumb-but-real register machine *inside the game `.exe`*. It has no
variables, no `if`, no `expr`, no loops — it only executes `ProcName reg=val,…` lines
against the live process, captures the register file, and answers over a channel (§6).
**Tier 2** is the other half: a standalone **Rust tool** — *adapted from `rust_tcl`* — that
runs **full TCL** on the **developer's** machine (`set`, `expr`, `if`, `while`, `foreach`,
`proc`) and drives the running game over Part 1's channel.

What it becomes is a **high-level scripting probe**: you script the game's globals in a
real language, read back registers, watch its framebuffer live, and — when a poked proc
faults — catch the exception dump, all over one channel. The split is the whole idea:

> **Smart TCL in Rust. Dumb register-machine execution in the live game.**
> Variables, control flow, expression evaluation, and *all formatting and display* happen
> *locally* in the tool. Only a fully-substituted `ProcName reg=val,…` line crosses the
> wire; only raw captured registers — and unsolicited events (output, state, faults) —
> come back. The game never learns what a variable is.

`rust_tcl` already exists and is embedded in studio, so the developer gets a real language
for free, and Tier 1 stays honest — it never creeps toward being a language in asm.

## 11. The tool — `nanotcl` (the Tier-2 driver)

A small Rust binary (`crates/nanotcl/`): concretely **`rust_tcl` + a connection + a
handful of registered verbs**, reused exactly the way studio does it
(`crates/studio/src/bin/studio.rs:3012` — `Registry::with_core()`, `r.register(verb,
arity, closure)`, run with `rust_tcl::eval`). The only new code is the connection (the
pipe client), the verbs that forward a call and read back registers, and the line codec.

```
crates/nanotcl/
  Cargo.toml      ; dep: rust-tcl = { path = "…/doccrate/crates/rust-tcl" }  (zero-dep, standalone)
  src/main.rs     ; arg parse: connect|attach|spawn, then REPL or run a .tcl file
  src/conn.rs     ; the wire client + the demux I/O thread (§13)
  src/verbs.rs    ; connect/attach/spawn/disconnect/call/reg/expect/step/puts-bridge (§17)
  src/wire.rs     ; the id-tagged line protocol encode/decode (§14)
  src/view.rs     ; the optional 60 fps dashboard (§15)
```

The tool has **three roles** (§13): an **eval thread** that runs the script, a **demux I/O
thread** that owns the channel, and an optional **60 fps display** that renders live state.
It runs in two modes:

- **REPL** — read a line, `eval` it, print the result + any output.
- **Script** — `nanotcl run poke.tcl` — read the file, `eval` it, exit non-zero on an
  `expect` failure (a per-proc regression test in a script).

> **REPL state — get this right.** `rust_tcl`'s `set`/`proc` live in the **`Vm`**, not the
> `Registry`, and `eval` builds a *fresh* `Vm` per call (`vm.rs`, `lib.rs:31`). So a naïve
> per-line `eval` would forget every variable and `proc` between lines. Keep session state
> the way studio does: **accumulate the session source and re-`eval` the whole buffer**
> each line (or hold a persistent `Vm`, if `rust_tcl` grows a run-against-existing-state
> entry — it has none today). Only the registered *verbs* (which live in the `Registry`)
> persist for free.

## 12. Connection model — how the tool finds the game

Three ways to get a live channel; all converge on one `Conn`.

| verb | what it does | when |
|---|---|---|
| `connect ?name?` | open the existing pipe `\\.\pipe\nanotcl` (default name) | game already running with `NANOTCL_LIVE` |
| `attach <pid>` | find the game's HWND from a pid, use **`WM_COPYDATA`** | game running but pipe disabled / GUI-only build |
| `spawn <path.exe> ?args?` | `CreateProcess` the dev build, wait for the pipe to appear, then `connect` | one-shot "boot it and poke it" |

**The named pipe is the primary transport.** It is duplex, so it carries the *full async
message stream* (§13–§14): id-tagged replies **and** unsolicited events (output, streamed
state, exception dumps). `WM_COPYDATA` (`attach`) is the **degraded fallback** — one
synchronous `LRESULT` per call, so `reg` returns only `eax`, with no events and no
dashboard. It's fine for a fire-and-forget `call` or a pass/fail `expect`; the rich probe
needs the pipe. The tool picks the transport from whichever verb opened the connection and
stores it in `Conn`; every other verb is transport-agnostic above that.

`disconnect` closes the handle and clears the global `Conn`, which lives in an
`Arc<Mutex<Option<Conn>>>` cloned into every verb closure — `rust_tcl`'s handler type is
`Fn(&mut Vm, &[Value]) -> Result<Value> + Send + Sync + 'static`, so shared wire state must
be behind an `Arc<Mutex<…>>` (the thread-safe analog of studio's `thread_local` `App`
pointer; this is the real I/O handle).

## 13. The frame-paced async model

The two tiers are **two 60 fps loops**. The game services channel traffic at its frame
boundary (`TclPoll` drains the queue inside `GameFrame`, §6), so a request sent now is
executed on the *next* frame and its reply lands ~1 frame (≤2) later. **The wire is
therefore an async, id-correlated *message stream*, not a blocking call/return** — a naïve
request→block-on-`ReadFile`→reply would hang forever on a paused or dead game. Three roles
split the work in the tool:

- **eval thread** — runs `rust_tcl::eval`. The script is *synchronous* (`set n [reg eax]`
  must return a value), so a wire verb sends its request and **blocks — with a timeout** —
  until the matching reply arrives.
- **demux I/O thread** — owns the pipe. Writes id-tagged requests; reads frames and
  **routes by id**: a `REPLY <id>` wakes the waiting verb (a per-request oneshot keyed by
  id); an unsolicited `EVENT` goes to the output log / the display. This demux is exactly
  what lets the game stream output and faults *between* replies.
- **60 fps display** (optional, §15) — renders the live dashboard from the event stream.

**Synchronous TCL over an async wire**, concretely: a `call` closure assigns the next id,
registers a oneshot under it, sends `<id> CALL …`, then waits on the oneshot **for ~N
frames**. On `REPLY <id>` → return the value; on timeout or pipe EOF → `Err("game
unresponsive / disconnected")`, which surfaces as a clean TCL error instead of a hang
(the liveness guarantee a raw blocking `ReadFile` lacked). The **frame is the unit of
latency and of backpressure**: at most one request round per frame, so a runaway script
can't outrun the game.

## 14. The wire protocol — id-correlated messages

Tiny, line-oriented, ASCII. Every request carries a **monotonic id**; every reply echoes
it; unsolicited messages are **events**. Three kinds:

```
REQUEST (tool → app)   7 CALL Pset rcx=10, rdx=20, r8=14
                       8 REG  eax            |   8 REG *
                       9 STEP
                      10 PUTS eax
REPLY   (app → tool)   7 OK eax=5            |   7 ERR <message>
                       8 reg=xmm0 1.4        (only in answer to REG)
EVENT   (app → tool)   * out: note count = 5            (a puts)
                       * frame 1421 320x200 …           (streamed state, §15)
                       * exc: ACCESS_VIOLATION @ 0x… rcx=0 …   (a fault, §15)
```

| field | rule |
|---|---|
| **id** | the tool's monotonic request number; the app copies it into the matching reply so the demux can correlate. Events use `*` (no id) — they belong to no request. |
| **CALL tail** | *byte-for-byte the Tier-1 statement syntax of Part 1* — the tool just prepends the keyword; the app strips it and hands the rest to the existing `txtPtr` parse loop. Zero new asm parsing. |
| **status** | `OK`/`ERR`, always carrying `eax` inline (`OK eax=5`) — the return value almost every call wants, free to append. Over `WM_COPYDATA` the single `LRESULT` *is* this `eax`. |
| **reg lines** | only in answer to `REG <name>`/`REG *`; the app formats the requested slot(s) from the still-current shadow file. |
| **events** | unsolicited, any time: `out:` (a `puts`), `frame` (streamed dashboard state), `exc:` (an exception dump, §15). The demux fans these to the display/log, never to a waiter. |

**Framing.** One message = one `WriteFile`, read whole — so the pipe is **message mode**
(`PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE`, §6) and one `ReadFile` returns exactly one
frame; or length-prefix each frame. (A "blank-line terminator" over a byte stream is
fragile — `out:` text can itself contain a blank line.)

**The leaner reply.** A `CALL` reply is **status + `eax` only**, never a 32-register dump:
`eax` is the one register a call almost always wants, and formatting 30 others every call
is dead weight (some via `VarBstrFromR8` + BSTR cleanup). `reg eax` is served free from the
cached status; `reg xmm0` issues one on-demand `REG` query against the persistent `.data`
shadow file (§3). `REG *` exists for a debug "dump all" but is off the hot path.

> The id echo, the `ERR` status, `REG`/`REG *` formatting, and the `out:`/`frame:`/`exc:`
> event writes are **new Tier-1 work** on the asm side — small but real, collected in §19.

## 15. The live dashboard & the exception channel

This is where "the game does no I/O" pays off: the game ships **raw register bytes and raw
events**, and the **Rust tool formats and renders everything**.

**The 60 fps dashboard.** Because the tool is itself a 60 fps loop, the display thread can
render the game's live state every frame from the event stream: a **register watch** (the
shadow file), an **output log** (`out:` events), and — if the game emits a `frame` event
carrying its `Snapshot`/framebuffer — a **live remote viewport** of the running game beside
the TCL prompt. studio *is* this dashboard already (it renders at display rate and owns an
output pane); the standalone CLI is the headless, frame-paced driver of the same protocol.
None of §5's Win32 numeric/print machinery runs on this path — it all becomes one-line Rust
formatting (int→string, double→string, hex, tables).

**The exception channel.** You poke procs with *arbitrary* args, so faults are *expected*,
not exceptional. Rather than let a bad call kill the process (§8), the dev build installs a
**vectored exception handler** (`AddVectoredExceptionHandler`): on a fault it captures the
`EXCEPTION_RECORD` + `CONTEXT`, formats a dump (fault code, faulting address, the captured
registers), writes it down the *same* channel as an **`exc:` EVENT**, and returns `ERR` for
the in-flight `CALL` so the session survives. The probe then *shows* the crash — fault,
address, register state — live in the dashboard, and you keep scripting. That is what makes
nano-TCL a **probe** rather than a remote control: it drives, inspects, watches, *and*
reports faults, all on one wire.

## 16. The split, end to end — a worked round-trip

Tool-side TCL (full language) and what actually crosses the wire (`id KIND …` out,
`id STATUS …` back):

```tcl
connect                          ; opens \\.\pipe\nanotcl                  [no wire I/O]
set x 10                         ; LOCAL — rust_tcl variable                [no wire I/O]
set y [expr {$x * 2}]            ; LOCAL — expr in the tool → y = 20        [no wire I/O]

Pset rcx=$x, rdx=$y, r8=14       ; substituted LOCALLY, then forwarded
                                 ;   → 7 CALL Pset rcx=10, rdx=20, r8=14   ← 7 OK eax=0
ParseTune rcx=tuneStr            ;   → 8 CALL ParseTune rcx=tuneStr         ← 8 OK eax=37
set n [reg eax]                  ; reg eax → cached from reply 8, n = 37    [no wire I/O]
puts "notes: $n"                 ; LOCAL puts in the tool's stdout          [no wire I/O]

for {set i 0} {$i < 8} {incr i} {            ; LOCAL loop — runs in the tool
  Beep rcx=[expr {440 + $i*40}], rdx=80      ;   → CALL Beep rcx=440,rdx=80 … rcx=720,rdx=80
  step                                        ;   → STEP  (advances one game frame per pass)
}

Add rcx=2, rdx=3                 ;   → CALL Add rcx=2, rdx=3                 ← OK eax=5
expect [reg eax] 5               ; reg eax cached = 5; compared LOCALLY → PASS
```

`expr`, `incr`, `if`, `for`, `$var` substitution — all in the Rust tool; only the
substituted `CALL`/`STEP` lines cross the wire, one round per frame.

> **Cache caveat.** The cached `eax` is the *last status line's* — a `STEP` (or any later
> `CALL`) overwrites it. So read `set n [reg eax]` *immediately* after the `CALL` you mean,
> before any `step`. For any other register, `reg xmm0` issues its own `REG` query and
> reads the shadow file as-of-now (§14).

## 17. The tool verbs (layered over `rust_tcl`)

All registered with `r.register(name, arity, closure)`; each closure clones the shared
`Arc<Mutex<Option<Conn>>>`. (`set`/`puts`/`if`/`while`/`foreach`/`proc`/`expr` already come
from `Registry::with_core()` — only the wire verbs are new.)

| verb | arity | behaviour |
|---|---|---|
| `connect ?name?` | 0–1 | open the pipe; store a pipe `Conn`. Default name `nanotcl`. |
| `attach <pid>` | 1 | resolve the HWND, store a `WM_COPYDATA` `Conn` (degraded; §12). |
| `spawn <exe> ?args?` | 1+ | `CreateProcess`, poll for the pipe, then `connect`. |
| `disconnect` | 0 | close + clear the `Conn`. |
| `call <Proc> ?reg=val …?` | 1+ | **explicit** forward: build `CALL …`, send, await the reply by id, cache `eax`, return `eax` as the TCL value. |
| `reg <name>` | 1 | return a captured register **as a TCL value**: `eax` from cache; otherwise send `REG <name>`. This is what makes `set n [reg eax]` work. |
| `regs` | 0 | send `REG *`, return a TCL dict of `name→value` (debugging). |
| `step` | 0 | send `STEP`; return `eax`. |
| `expect <a> <b>` | 2 | resolve both **locally**, compare, set a non-zero process exit on mismatch + a diagnostic (the script doubles as a test — comparison runs in the tool). |
| `puts <expr>` | 1 | the `with_core` `puts` — prints in the tool's own stdout. Game-side `puts` arrives as `out:` events and is surfaced by the demux (printed and/or appended to `RunResult.output`). |

### Forwarding: explicit `call`, plus discovered-verb sugar

Recommend the explicit `call` verb as the always-available contract (`call Pset rcx=10,
rdx=20`) — it needs *no* change to `rust_tcl`. The ergonomic sugar (typing the bare proc
name as in Part 1, `Pset rcx=10, …`) comes from **auto-registering the game's procs as
forwarding verbs — at `connect`, *before* `eval` runs.** This timing is **not optional**:
`rust_tcl`'s `Vm` borrows the `Registry` *immutably* for the whole run (`vm.rs:23`), so a
closure **cannot** `r.register` mid-`eval`. So `connect` is a startup step — open the
channel, learn the proc names (a one-shot `VERBS` query, or read the `was --emit-verbs`
table, §4), `r.register(<Proc>, at_least(0), forward)` for each, *then* start the REPL /
`eval`. (A general catch-all — fall through `rust_tcl`'s unknown-command error at
`vm.rs:328` to a forwarder — is one upstream line, deferred behind the auto-register
approach, which needs nothing upstream.)

Arguments pass through verbatim: `rust_tcl` does `$var` substitution before the verb runs;
the verb re-joins the words into a `CALL` line; **Tier-1's asm lexer (§2) does the real
`reg=val` parsing**. One parser, on the asm side, exactly as Part 1 designed.

## 18. studio reuse — one registry, two front-ends

studio **already** embeds `rust_tcl` to drive the IDE (`script_registry()` in `studio.rs`).
The Tier-2 verbs are the *same shape* — `r.register(name, arity, closure)` — so studio can
host a **"connect to a running game"** mode by registering the §17 verbs into *its* registry
alongside the existing `type`/`click`/`screenshot` IDE verbs:

- **Factor the wire verbs into a reusable function** `nanotcl_verbs::register(&mut Registry,
  Arc<Mutex<Option<Conn>>>)` (in `crates/nanotcl/src/verbs.rs`, or a small `nanotcl-client`
  lib crate both binaries depend on). The CLI calls it on a bare `Registry::with_core()`;
  studio calls it on its existing `script_registry()`.
- Then **one studio session can do both**: script the IDE *and* poke a game it launched —
  `open game.was`, build, `spawn out/game.exe`, then `Pset rcx=10, …` and read `reg eax`,
  all in one TCL console, with `out:` text and the `frame`/`exc:` dashboard landing in
  studio's panes (studio is "the natural client" per §6 — this makes that literal, and it
  is *already* a 60 fps renderer, so it is the §15 dashboard for free).
- **So it is not a separate tool *or* studio — it is one set of verbs with two front-ends.**
  The standalone `nanotcl` CLI exists for headless/CI use (run `poke.tcl`, non-zero exit on
  an `expect` failure); studio reuses the identical verbs interactively. Both forward the
  same `CALL`/`REG`/`STEP` lines to the same Tier-1 executor.

## 19. Tier-1 additions Part 2 needs

All small, all on the asm side (Part 1), all already implied above — they live in
`inject.was`/`repl.was`, not the trampoline or the language:

- **keyword strip** — recognise `CALL`/`REG`/`STEP`/`PUTS` prefixes, strip, hand the tail
  to the existing `txtPtr` loop.
- **id echo** — carry the request id from the request line into its reply line.
- **`REG` / `REG *` / `ERR`** — format an arbitrary slot (or all non-zero slots) from the
  shadow file on demand; emit `ERR <msg>` on a bad verb name / parse error.
- **events** — write `out:` (the redirected `puts`, §5/§6), and optionally `frame` (stream
  the `Snapshot`) and `exc:` (the VEH dump, §15).
- **message-mode pipe** — `PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE` so one write = one
  frame (§14).

## 20. Tier-2 limits & risks

- **Per-call latency ≈ one frame.** At 60 fps each forwarded call costs ~16 ms round-trip.
  A loop *condition* that reads the wire every iteration (`while {[reg eax] < N} …`) pays it
  each pass — keep loop conditions on **local** vars and forward inside the body; batch
  where you can.
- **Cache is last-status-only** — §16; a `step`/`CALL` between a `CALL` and its `reg eax`
  invalidates the read.
- **Liveness** — every wire verb blocks with a timeout (§13); a dead/paused game yields a
  TCL error, not a hang. The `exc:` channel (§15) reports faults instead of crashing the
  session.
- **REPL persistence** needs the accumulate-and-re-eval affordance (§11) — `rust_tcl` keeps
  vars/procs in the `Vm`, not the `Registry`.
- **One client at a time** — Part 1's pipe is `nMaxInstances=1`; the CLI and a studio
  session can't both hold it; `connect` fails fast if it's busy.
- **Transport asymmetry** — `attach`/`WM_COPYDATA` carries only `eax`, no events/dashboard;
  the tool warns when a script needs `reg <non-eax>`, `regs`, `out:`, or `frame`.
- **Dev-only, inherited** — the tool is a *client* of the §6 gated channel; nothing in
  Tier 2 weakens that gate.

## 21. Tier-2 implementation phases

| phase | build | effort |
|---|---|---|
| **T0 — pipe client + `call`/`reg`** | `crates/nanotcl` over `rust_tcl`; `connect` + the demux thread + `call` (send `CALL`, await `OK eax=` by id) + `reg eax` (cached). Drive a running `NANOTCL_LIVE` game; prove `call Pset …` lights a pixel and `set n [reg eax]` reads back. | ~1 day |
| **T1 — full round-trip** | the id-tagged §14 codec, `REG`/`regs`, `step`, `out:` events surfaced, `expect` (local, exit code), the block-with-timeout liveness path. | ~½ day |
| **T2 — discovery + sugar** | `VERBS` query (or read `--emit-verbs`); auto-register per-game verbs **at connect** so bare `Pset rcx=…` forwards; `spawn`. | ~½ day |
| **T3 — the dashboard** | the 60 fps display thread: register watch + output log + a streamed `frame` viewport. | ~1 day |
| **T4 — the exception channel** | Tier-1 `AddVectoredExceptionHandler` → `exc:` events; the probe surfaces faults + register state and the session continues. | ~½ day |
| **T5 — `attach` + studio mode** | HWND-from-pid `WM_COPYDATA` (eax-only, degraded); factor `nanotcl_verbs::register` into studio's `script_registry()`; events to studio's panes. | ~½ day |

Rough total **≈ 700 LOC of Rust** (the §14 codec, the demux, the verb closures, the
dashboard), the small Tier-1 additions of §19, and — if the deferred catch-all is ever
wanted — one line in `rust_tcl`'s `vm.rs`.
