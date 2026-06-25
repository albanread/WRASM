# nano-TCL — breakpoints on a proc (design proposal)

> **Status: design only — not built.** A companion to [nano-tcl.md](nano-tcl.md)
> (the original design) and [nano-tcl-sprints.md](nano-tcl-sprints.md). It proposes
> how to add *"set a breakpoint on a proc"* to the **already-built** nano-TCL
> executor, which lives at [`library/tcl/tcl.was`](../library/tcl/tcl.was)
> (module `Tcl`) — **not** the `library/nanotcl/` path the original design doc
> sketched (that directory doesn't exist). The symbols cited below are from the
> real code.

## 0. The question

*Can nano-TCL set a break on a proc?* **No** — but it is closer than it looks.

What's built today (`tcl.was`): a shadow register file (`shadowGP`), the
`nanoCall` load→call→capture trampoline, the `Verb reg=val,…` bare-line parser
(`parseOne`/`RunTcl`), a name→address verb table (`verbTbl` + `TclAddVerb` +
generated `stdverbs`), and — gated behind `NANOTCL_LIVE` — a **live channel**: a
named pipe, a reader thread (`TclReader`), and a frame-boundary drain (`TclPoll`).

Crucially, that channel already includes a **frame-sync pause**:

- `intro rcx=N` (`IntroOn`) → `introMode=1`: every `N` frames, `TclPoll` calls
  **`TclFrameSync`**, which **blocks the game thread at the frame boundary**,
  pings the 16 shadow registers (hex) down the pipe, and waits.
- `cont` (`IntroCont`) → sets `gotCont=1`, releasing the held frame.
- `free` (`IntroFree`) → back to free-running.

So nano-TCL **can already stop the world, show registers, and continue.** What it
*cannot* do is trigger that stop **when the game's own code reaches a particular
proc**, with that proc's live arguments.

## 1. What's missing: a *location* trigger

The existing pause is **temporal** — "stop every N frames." A breakpoint is
**spatial** — "stop when control reaches `Pset`." The difference is the whole
feature:

| | trigger | inspect | built? |
|---|---|---|---|
| `intro N` / `cont` | a frame counter (`introEvery`) | the shadow slots the app publishes via `TclReg` | ✅ `tcl.was` |
| **break on a proc** | execution reaches `Pset` | **`Pset`'s real incoming `rcx/rdx/r8…`** that frame | ❌ this doc |

The use case the frame-sync pause can't serve:

> *"Let the game run. The moment **the game itself** calls `Pset`, stop, and show
> me the `rcx/rdx/r8` it was about to pass."*

`call Pset rcx=10,…` (which works today) invokes `Pset` with args *you* supply.
A breakpoint shows the args **the game** supplies — the thing you can't see any
other way.

### An introspector, not a debugger

nano-TCL is deliberately **proc- and frame-focused**, not instruction-focused. Its
two granularities are the **proc** (break and profile at a named proc's entry) and
the **frame** (the existing `intro`/`cont` pause at the frame boundary, a safe
state). It does **not** do arbitrary-address breakpoints or single-instruction
stepping — those are general-debugger features that drag in `INT3`/debug-register/
VEH machinery and the half-executed, mid-instruction states that come with them.
Working at proc + frame granularity is *why* a compiled-in entry hook (not a trap
mechanism) is the right and sufficient design — and why you never stop in a
half-finished state.

## 2. The design in one line

> **The `proc` macro compiles a register-free tally + countdown into every proc
> under `NANOTCL_LIVE`. The TCL verbs poke a per-proc metrics table. Each proc
> profiles every call and breaks *itself* when its countdown reaches zero, then
> re-arms from the table.**

This keeps Tier-1 genuinely dumb: a proc does almost nothing on the hot path —
*tally and count down* — and at zero calls the pause and reloads. **All policy lives
in the table**
the verbs write; the interpreter never learns what a breakpoint is. It is the
compile-time realisation of "mechanism C" (a hook in the prologue) — and the most
WRASM-idiomatic option: the instrumentation is *in the listing* (`--emit-asm`),
gated out of release, with no VEH, no debug registers, and no self-modifying code.

Three things have to be true, in priority order:

1. **Inert when disarmed.** The normal path must have *no observable effect on the
   proc but a small delay* — no clobbered registers, no disturbed flags, no memory
   the proc can see. (§4 — the heart of the design.)
2. **Inert when it fires, too.** A hit changes *timing only* — the proc resumes
   with byte-identical register state, unless you deliberately `set` one while
   stopped. (§5.)
3. **Free of new runtime machinery.** The pause/continue half already exists; reuse
   it. (§5, the `TclFrameSync` reuse.)

## 3. The metrics table — `{ hits, hitcount, hittrigger }` per proc

One generated table, **three hot fields per instrumented proc**, emitted in the
same pass that assigns each proc its row index (parallel to how `verbTbl` is built).
Per the no-hidden-codegen rule it **prints the rows it emits**, like `--emit-verbs`.

```asm
.balign 16
hits       QWORD N dup(0)                   ; the PROFILER — total calls, counts UP. `hits Pset` reads it.
hitcount   QWORD N dup(0x7FFFFFFFFFFFFFFF)  ; the LIVE down-counter — "calls until the next break".
                                            ;   the hook dec's it; 0 => break. init = sentinel (never fires).
hittrigger QWORD N dup(0x7FFFFFFFFFFFFFFF)  ; the RELOAD value (period N). re-armed FROM here on a break.
procName   QWORD N dup(0)                   ; cold: asciz name ptr per row — ping header + name→row lookup;
                                            ;   never touched on the hot path.
```

- **`hits[ROW]`** — the profiler: total calls, counts up; `hits Pset` reads it. It
  is a *separate* field precisely because a register-free *break* test must count
  down (§4), so the running total gets its own up-counter.
- **`hitcount[ROW]`** — the live counter the hook decrements; reaching 0 is the
  break. Counts down (the register-free test, §4).
- **`hittrigger[ROW]`** — the period the break reloads from, **only on a break**
  (§5). `5` = "break every 5 calls"; the sentinel = "never again" (one-shot).
- **`procName[ROW]`** — cold metadata; read only when arming or on a hit.

`N` is the proc count the macro assigned; `ROW` is each proc's compile-time index,
so `hits + ROW*8` is a **constant displacement** — no runtime index register. (The
three hot fields can interleave as one 24/32-byte struct so a proc's counters share
a cache line; shown as parallel arrays here for clarity.)

## 4. The register-free countdown hook (the heart)

The `proc` macro emits this at the very top of every proc under `NANOTCL_LIVE`,
*before* the proc's own prologue. The smart part is that the disarmed path **touches
zero general-purpose registers**:

- the address `hitcount + ROW*8` is a **RIP-relative constant displacement** (ROW
  folded in at assemble time) — no register to form the address;
- a **`dec [mem]; jnz`** decides the break with **no compare** — `dec`-to-zero sets
  `ZF` without a scratch register, the one trick that removes the GP clobber (the
  `inc [hits]` profiler tally is register-free the same way);
- `pushfq`/`popfq` bracket the `inc`/`dec`, so **flags are provably restored**.

```asm
Pset:                                              ; under NANOTCL_LIVE only
    pushfq                                          ; preserve flags (the only volatile state we touch)
    inc   qword ptr [rip + hits     + PSET_ROW*8]   ; profiler tally      -- no GP register
    dec   qword ptr [rip + hitcount + PSET_ROW*8]   ; "calls until break" -- no GP register; ZF=1 at zero
    jnz   Pset__resume
    popfq                                           ; restore flags before the heavy path
    sub   rsp, 40                                   ; 32 shadow + 8 pad -> realigns the call (see below)
    mov   r11, PSET_ROW                             ; which proc (r11: volatile, non-arg -> invisible)
    call  TclBrkHit                                 ; args rcx/rdx/r8/r9 still live -> captured, pause, re-arm
    add   rsp, 40
    jmp   Pset__body
Pset__resume:
    popfq                                           ; flags restored -- the proc sees ZERO change
Pset__body:
    push  rbp                                       ; ... the real prologue + body, unchanged ...
```

**Disarmed path = `pushfq · inc · dec · jnz · popfq` — 5 instructions, 0 GP
registers clobbered, flags restored, two private qwords written.** Nothing the proc
body or its caller can observe but a few cycles of delay. That is the "no effect but
a delay" requirement, met literally.

**Why it's provably inert.** The only volatile state the disarmed path disturbs is
RFLAGS, and it is saved/restored. No GP register is read or written. The two memory
writes hit private counters no proc references. `r11` and the `sub rsp,40` appear
**only on the taken (breaking) branch**, where we are pausing anyway.

**Re-arm is *not* on the hot path.** The hook only decrements; the reload
(`hitcount = hittrigger`) happens inside `TclBrkHit`, on a break only (§5). So a
disarmed proc never pays for the re-arm logic.

**Stack alignment across the break path** (entry `rsp ≡ 8 mod 16`):
`pushfq → 0`, `popfq → 8`, `sub rsp,40 → 0` (so the `call` is 16-aligned),
`call` pushes the return address → callee enters at `≡ 8` ✓, `add rsp,40 → 8`.
Both branches reach `Pset__body` with `rsp ≡ 8` — exactly a normal proc entry —
so the real prologue runs identically.

> **Needs a `was` change.** Emitting this hook for every proc under `NANOTCL_LIVE`
> is a `proc`-macro/codegen change — that half lands in `was` (kin to
> `was --emit-verbs`). For a first proof the hook can be **hand-written** in one
> proc with no `was` change (§9). The `hitcount`/`hittrigger`/`procName` table,
> `TclBrkHit`, and the verbs are the `library/tcl` side.

## 5. The hit handler — capture, pause, re-arm

`TclBrkHit` runs only on the rare taken branch, with the proc's live arg registers
still intact. It is `TclFrameSync` (the existing pause) with a register snapshot in
front, a re-arm at the back, and a full save/restore around the lot:

1. **Save everything it will touch** — push all GP + RFLAGS, save the xmm arg
   registers — so the proc resumes byte-identical (requirement 2).
2. **Capture** the incoming args (`rcx/rdx/r8/r9`, xmm0–3) into `shadowGP` — the new
   bit. (The frame-sync path samples app-published state via `TclReg`; here we
   sample the live call-site registers.)
3. **Hand the pipe to the game thread** — set the reader-park flag (reuse
   `introMode=1`, or a `brkMode` sibling) so `TclReader` parks (`tr_park`).
4. **Ping** `brk: Pset @… rcx=… rdx=… …` (the existing hex-register ping, name from
   `procName[r11]`).
5. **Block for `cont`** — the exact `fs_wait` loop: `ReadFile → RunTcl → repeat`
   until `gotCont`. While blocked you `set`/inspect; `cont` releases.
6. **Re-arm from the table** — `hitcount[r11] = hittrigger[r11]`. **This is the only
   place re-arm happens.** A finite `hittrigger` → recurring (break every N calls);
   a sentinel `hittrigger` → one-shot (it won't fire again).
7. **Restore everything** and return — `Pset__body` runs with whatever the
   registers now hold (unchanged unless you `set` one).

A **logpoint** (`break -log`) skips steps 3/5: capture, ping, re-arm, return — a
non-stopping "show me `Pset`'s args every N calls" at 60 fps.

Because step 2 lands the registers in `shadowGP`, the existing register ping shows
the breakpoint's state with **no new formatting** — the single biggest reuse.

> **The pump freezes while stopped** — no repaint mid-frame; expected at a
> breakpoint (already true during an `intro` block). A "STOPPED" banner tool-side
> keeps it from reading as a hang.

## 6. The verbs poke the table

In the same bare-line style the channel already parses, registered in `TclInit`
beside `intro`/`free`/`cont` (inside the existing `IFDEF NANOTCL_LIVE`):

| verb (Tier-1, `tcl.was`) | effect |
|---|---|
| `brk Pset n=5` | `hittrigger[row]=5; hitcount[row]=5` — break every 5th call (recurring) |
| `brk Pset` | `hittrigger=sentinel; hitcount=1` — break on the next call, **one-shot** |
| `unbrk Pset` | `hitcount=hittrigger=sentinel` — disarm (the `hits` tally keeps counting) |
| `hits Pset` | read back `hits[row]` — the profiler (total calls) |
| `cont` *(exists)* | already releases a held frame — **reused verbatim** to resume |

`brk`/`unbrk` resolve `Pset → row` by scanning `procName` (or via `verbTbl` if the
macro keeps the two row-aligned). EVENT line (unsolicited, like the `frame ` ping):
`brk: Pset <16 hex regs>`.

Tool side (`crates/nanotcl`, over `rust_tcl`, per nano-tcl.md §17):
`break <Proc> ?n? ?-log?`, `unbreak <Proc>`, `breaks`, `hits <Proc>`, `continue`.
The tool does any target arithmetic in full TCL (the two-tier split).

## 7. Decisions pinned

- **Recurring by default; one-shot is the special case.** Re-arm-from-table (§5
  step 6) makes "break every N calls" the natural behaviour — `hittrigger` *is* the
  period. A one-shot is just arming with `hittrigger = sentinel` (the reload then
  disables it).
- **Two counters because the break must count down.** The register-free break test
  needs `dec;jnz`, so `hitcount` is "calls remaining," not a total — the profiler
  `hits` is a separate up-counter. Both are register-free; the hot path pays two
  memory writes.
- **Targets are relative; the tool does the math.** A countdown is "calls from now"
  for free. "Break on the *Nth ever*" = the tool seeds `hitcount = N − total` (if it
  tracks a total), in Rust.
- **Disarmed = a big sentinel, never 0.** The table inits both fields to
  `0x7FFF_FFFF_FFFF_FFFF`; a disarmed proc won't reach zero in any real run. The
  hook still `dec`s it (the value drifts down harmlessly); `unbrk` resets it.
- **Tool-injected `call`s decrement but don't self-break.** A scripted `call Pset`
  runs through `nanoCall → call [gVerbPtr] → Pset`, hitting the same hook. Set an
  `inNanoCall` flag around `nanoCall`'s indirect call; `TclBrkHit` checks it on the
  taken branch and returns without pausing/re-arming. The hot path stays 4
  instructions.

## 8. The alternatives, and why the hook wins now

| # | Mechanism | Inert when disarmed | Pristine code | No recompile | Needs a VEH |
|---|---|---|---|---|---|
| **C** | **Compiled-in countdown hook** (this doc) | ✅ 0 GP, flags saved | ✅ + visible in `--emit-asm` | ❌ | ❌ |
| **A** | Hardware debug regs `Dr0–Dr3` → `#DB` | ✅ | ✅ | ✅ | ✅ |
| **B** | `INT3` patch (`0xCC`) → `#BP` | ✅ | ❌ mutates running code | ✅ | ✅ |

A and B are **instruction-level** mechanisms: they trap at an arbitrary address,
need a vectored exception handler for `#DB`/`#BP`, and expose the half-executed
states that come with mid-instruction traps. That is a *general debugger's* model —
and explicitly **not** nano-TCL's (§1: proc- and frame-focused). The compiled-in
entry hook is **not a fallback for "no VEH yet"; it is the mechanism that matches
proc granularity** — it stops only at a proc entry, on the cooperative frame-safe
pause, never half-finished. A remains a conceivable later add-on **only if** ad-hoc
breakpoints on un-hooked code are ever wanted (and only once the `exc:` VEH exists,
nano-tcl.md §15); B (self-modifying `0xCC`) is ruled out by the transparency ethos
regardless.

## 9. Smallest first cut (no `was` change, no VEH)

Prove the spine before touching `was` codegen:

1. **Hand-write the countdown hook in one proc** (§4) in a `NANOTCL_LIVE` game, with
   a hand-written `{hits, hitcount, hittrigger}` row.
2. **`TclBrkHit`** = save-all → capture `rcx/rdx/r8` into `shadowGP` → `introMode=1`
   (park the reader) → reuse `TclFrameSync`'s ping + block-for-`cont` → reload
   `hitcount = hittrigger` → restore-all → return.
3. **Add `brk`/`unbrk` verbs** to the `NANOTCL_LIVE` block of `TclInit`.
4. **Demo:** run the game, `brk Pset n=3`, watch it stop on every 3rd call with the
   game's real `rcx/rdx/r8`, `cont` between hits — entirely on the existing
   cooperative channel, no exceptions, no codegen change.

Then the additive layers: the `was`-emitted hook for *every* proc, `-log` logpoints,
the `inNanoCall` gate, and — only if ad-hoc no-recompile breakpoints are ever
wanted — the hardware-`Dr` path (A) when the `exc:` VEH lands.

## 10. Hazards

- **The reader-park race.** Arming during free-running must move the channel into
  reader-parked state *before* a hit lands, so the game thread can own the pipe (the
  `intro` path sidesteps this by sending `intro` first). Simplest: `brk` also sets
  the park flag, like `intro` does.
- **Don't `call` while stopped.** The blocked thread is the game thread; an injected
  `call` would run game code on the **reader thread** — unsafe for single-threaded
  state. Contract while stopped: inspect / `set` / `cont` only.
- **One stop at a time.** Ignore a nested hook hit while already stopped (v1).
- **Pipe-only.** The stop needs the duplex pipe + the parked reader; the degraded
  `WM_COPYDATA`/`attach` transport (nano-tcl.md §12) can't host it. (The built
  channel is pipe-only today anyway.)
- **Per-call cost is real but gated.** Every instrumented proc pays `pushfq` + two
  memory RMWs + a branch + `popfq` on every call — acceptable **because it's
  `NANOTCL_LIVE`-only**; release is byte-identical to today (the hook is inside the
  `IFDEF`, emitted by the macro only when the flag is set).
- **Dev-only, inherited.** Same `NANOTCL_LIVE` gate as the rest of the channel; a
  breakpoint subsystem is even more obviously a debugger. No weakening.

## 11. Where it slots

A small extension of the **live channel**, not a new pillar — it reuses
`TclFrameSync`, `introMode`/`gotCont`, `shadowGP`, and the table-build pattern of
`verbTbl`. Sequencing against [nano-tcl-sprints.md](nano-tcl-sprints.md):

- **Now-ish (cooperative, no VEH):** §9's first cut + the `was`-emitted countdown
  hook can land as soon as the live channel is solid — it does **not** wait on the
  `exc:` VEH (Sprint 7a). Rough effort: the `proc`-macro hook + table in `was`
  (your side), plus ~1 day asm (`TclBrkHit`) + ~½ day Rust verbs (the `library/tcl`
  + driver side).
- **Later (no-recompile):** mechanism A (hardware `Dr` + `#DB`) rides in with the
  Sprint-7a VEH, adding ad-hoc breakpoints on code the macro never hooked.

The headline: **the stop/continue machinery already works in the frame-sync pause;
a proc breakpoint is a register-free countdown the `proc` macro compiles in, a
`{hits, hitcount, hittrigger}` table the verbs poke, and the existing pause it calls
at zero — profiling every call, re-arming from the table on break, provably inert
until it fires.**
