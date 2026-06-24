# nano-TCL — build plan (sprints)

The execution companion to [nano-tcl.md](nano-tcl.md) (the design). The design says
*what* to build; this says *in what order*, *what each step demos*, and *when it's done*.

**Shape.** A **walking-skeleton spine** — the thinnest TCL→pixel→register round-trip
first, thickened layer by layer — with exactly one risk-first detour: a **Sprint 0** that
spikes the only assumptions that can force an architecture change. The product arc is
**CLI-first, studio-last**: the standalone `nanotcl` CLI ships and is CI-capable by
Sprint 3; studio is the final graft (design §18 — one registry, two front-ends), never a
prerequisite.

**Method.** Each sprint is assigned to agents, **built, then reviewed once** — not
triple-verified. The deep adversarial treatment is reserved for the architecture-deciding
step (Spike-R1, the VEH catch-and-resume). This plan was itself produced by a
strategy-panel + de-risk + adversarial-critique pass; the critique's fixes are folded into
the DoD column below.

## Sprint table

| # | Goal | Demoable deliverable | Pulls from (P/T + spikes) | Effort | Exit / DoD |
|---|---|---|---|---|---|
| **0** | **Spikes — retire the redesign risks first** | Four throwaway harnesses, run live | Spike-R1 (VEH+manual-longjmp) · R3 (pipe framing) · R4 (REPL re-eval) · R-snap (framebuffer reachability) | ~1 day | **R1 fault fires *inside a real `nanoCall`-shaped frame* (live `rbp`, shadow space) — not a flat stub**; handler doctors `Rip/Rsp/Rbp/MxCsr`→`recover:`, returns `CONTINUE_EXECUTION`, survives a **second** fault; an ungated fault still crashes (no over-catch). Framing = **length-prefix**. Re-eval keeps `set x`; wire verbs excluded from replay. **Framebuffer (`fb`/Snapshot) address confirmed reachable from a new `inject.was`** (else Sprint 6 grows). None of this code ships as-is. |
| **1** | **Walking skeleton — one real call lights one real pixel** | From the `nanotcl` CLI against a running `NANOTCL_LIVE` game: `call Pset rcx=10,rdx=20,r8=14` lights pixel (10,20) colour 14; a **script file** `set n [reg eax]` → `0`. Captured BMP→PNG. | **P0** (shadow file + register lexer + `nanoCall` load/call/capture + hand-written 2-row table) + **P2-thin** (`WM_COPYDATA` only) + **T0-thin** (`connect`/`call`/`reg eax` over COPYDATA) | ~2 days | Runs as a **file (single `eval` — no REPL persistence yet)**. T0-thin sends the **bare statement, no `CALL` keyword yet**. `reg eax` reads the COPYDATA `LRESULT`; non-`eax` regs out of scope. `nanoCall` owns its `.balign 16` frame, flags-first capture, `rbp` survives. Channel assembles only under `NANOTCL_LIVE`. `--emit-asm` shows every interpreter byte. |
| **2** | **Duplex pipe + baked scripts + REPL persistence + dev-gate proven** | (a) Same call over the **length-prefixed named pipe** (reader thread + `xchg` spinlock queue + `WriteFile` reply); CLI auto-prefers the pipe. (b) A game-baked `.tcl` runs: `Pset…; step; puts eax` → `OutputDebugStringA` (DebugView). (c) Interactive REPL keeps `set x` across lines. | **P1** (value lexer int/hex/str/reg, *no float* + parse loop + `set`/`puts`(int)/`expect`/`step` + `tclBusy` guard) + **P2-full** (reader thread, length-prefix framing) + **accumulate-and-re-eval** (from R4) | ~2 days | `step` re-entry proven: baked `Pset; step; Pset; step` advances frames without clobbering `txtPtr`; `tclBusy` clears on every exit. **`NANOTCL_LIVE` byte-absence verified via an `--emit-asm` release diff** (the dev-only gate is *built and audited here*). |
| **3** | **Full async round-trip + protocol — first genuinely useful probe** | CLI runs the design §16 multi-line script (`ParseTune … set n [reg eax] … for {…}{Beep…; step} … Add; expect [reg eax] 5`) → **PASS**; a forced mismatch → non-zero exit. Game-side `out:` text shows inline. A killed/paused game → clean TCL error within the timeout, not a hang. | **T1** (id-tagged §14 codec, demux I/O thread, `REG`/`regs`/`step`, `out:` events, `expect`+exit code, block-with-timeout) + **§19 Tier-1 additions** (keyword-strip CALL/REG/STEP/PUTS, id echo, `REG`/`ERR` formatting, `out:` write) | ~2 days | §16 round-trip runs verbatim and passes. **Keyword-strip lands here** (S1's bare-line path retired). **Negative test: `CALL; step; reg eax` asserts the cache-invalidation semantic** (the §16 caveat is pinned, not discovered later). Mid-script kill → TCL error within timeout. **CLI is now CI-capable.** |
| **GATE** | **CLI → studio gate** — *"once we are happy"* | *a decision, not a build* | — | — | Criteria below; authorises Sprint 7b only. |
| **4** | **Float — close the type story** | `Tone xmm0=1.4` lands a real double in `xmm0` + calls; `puts xmm0` → `1.4` (standalone) / ships raw for the tool to format; `expect` works on a float register. | **P4** (`VarR8FromStr` / `VarBstrFromR8`+`SysFreeString` / MBToWC+WCToMB / LCID `0x7F`) | ~1 day | `xmm0=1.4` round-trips; no BSTR leak. GP-vs-XMM dispatch proven (the "kills the float wall" claim). Independent of the gate; may run parallel to graft prep. |
| **5** | **Discovery + bare-verb sugar — generate the table** | `was --emit-verbs` prints + emits `verbTbl` (replaces the hand table), printing every row as it writes. CLI `connect` runs a `VERBS` query and auto-registers per-game procs **before** `eval`, so bare `Pset rcx=10` forwards. `spawn out/game.exe` boots-and-pokes. | **P3** (`was --emit-verbs` walking `parse_proc` names) + **T2** (VERBS query / register-at-connect / `spawn`) | ~1.5 days | `--emit-verbs` **reproduces the 2 hand-written rows identically and the row format matches** (not a whole-table diff). `HWndProc` excluded (a `.globl` label, not a `proc`). Verbs register before the immutable `&Registry` borrow. Fallback: one-line catch-all at `vm.rs:328`. |
| **6** | **The dashboard — a probe you watch** | CLI 60 fps thread renders a **register watch** (shadow file) + **output log** (`out:`) + a **live remote viewport** from a `frame` event — beside the prompt. Poke a proc → see the register change and the pixel appear the same frame. | **T3** (display thread) + **§19** `frame` write (shared-memory Snapshot + "frame ready" notify, latest-wins) | ~1.5 days | Streaming fits the 16 ms budget: framebuffer rides `CreateFileMapping`, main thread only flips a pointer; the control pipe stays tiny. The cheap panes (watch + log) ship even if streaming is throttled. |
| **7a** | **Exception channel** | A deliberately-bad poke (`Pset rcx=0` null-deref) is **caught**: the VEH ships an `exc:` dump (code, address, registers), the call returns `ERR`, the session survives and keeps scripting (or report-then-respawn, per the R1 verdict). | **T4** (VEH→`exc:`, queue-from-handler not `WriteFile`, range-gate, clear `tclBusy`) | ~1 day | The probe *reports* a crash instead of dying. **Split from the graft** so an asm slip can't block the studio demo; gated by the Sprint-0 R1 verdict. |
| **7b** | **Studio graft — ship the debugger** | `nanotcl_verbs::register(&mut Registry, conn)` is factored out; **studio** calls it on its existing `script_registry()`. One studio session: `open game.was; build; spawn out/game.exe; Pset rcx=10,…; reg eax`, with `out:`/`frame:`/`exc:` landing in studio's panes. | **T5** (`attach` degraded `WM_COPYDATA`; factor `register`; events → panes) | ~1 day | Depends only on the T1 verb closures — **can ship before 7a**. studio is the 60 fps dashboard for free. Design §18 (one registry, two front-ends) realised. |

**Totals:** ~7.5 days to a CI-capable CLI (Sprints 0–3 + float); ~12 days to the studio
debugger. Tracks the design's ≈460 LOC asm + ≈700 LOC Rust at ½–1 day/phase.

## The "happy with CLI → graft into studio" gate

Sits **after Sprint 3** (Sprints 4–6 harden the *same* CLI and may straddle it). All five
must hold before paying the graft cost:

1. **The §16 round-trip runs verbatim and `expect` gates a real regression** — PASS on a
   correct game, non-zero exit on a forced mismatch (CI-usable).
2. **The async wire is trusted** — REPLY-by-id demux correct, EVENTs fan to the log, and a
   killed/paused game yields a TCL error within the timeout: **zero hangs** across a stress
   run.
3. **Dev-only is provable** — the channel assembles only under `NANOTCL_LIVE` and is
   byte-absent from a release build (`--emit-asm` diff), built and verified in Sprint 2.
4. **The R1 verdict is locked** — either resume works (7a builds the live exception
   channel) or report-then-respawn is the accepted contract: no open architectural
   question rides into studio.
5. **The author has dogfooded it** — used the CLI on several real game procs and judged the
   `reg=val` ergonomics acceptable.

## Critical path & parallelism

**Critical path (the spine that cannot be shortcut):**

```
Spike-R1(S0) → P0 nanoCall(S1) → P2-thin(S1) → P1 parse loop(S2) → P2-full pipe(S2)
            → T1 demux+codec(S3) → [GATE] → T4(7a) / T5 graft(7b)
```

Spike-R1 sits on the path only as a **go/no-go**: a fail doesn't block — it swaps 7a's
contract to report-then-respawn, decided on day 0.

**Runs in parallel (fill solo-dev idle):** the R3/R4 spikes are pure Rust (independent of
the asm trampoline); `was --emit-verbs` (Sprint 5) is a self-contained `was` change with
no edge into P0–P2 (slot it into any Rust-blocked gap); P4 float (Sprint 4) is isolated
Win32 plumbing; the dashboard's cheap panes (Sprint 6) don't depend on the `frame`-stream
resolution; **7b can ship before 7a**.

## Risk register

| Risk | Retired | Net | Pre-committed fallback |
|---|---|---|---|
| **R1 — VEH catch-and-resume in CRT-free asm** (no `.pdata`/`.xdata`; manual longjmp via `EXCEPTION_CONTINUE_EXECUTION`, restore `Rsp`/`Rbp`/`MxCsr`, range-gate, clear `tclBusy`) | Spike-R1 (S0), built 7a | **CRITICAL** | Report-then-terminate; tool auto-respawns (T2 `spawn` + §13 liveness already exist). |
| **R2 — 60 fps framebuffer streaming stalls the pump** | S6 via shared-memory Snapshot + notify | HIGH | On-demand/throttled `frame`; indexed-8bpp+palette; watch+log panes ship regardless. |
| **R3 — pipe framing ↔ Rust** (partial/coalesced reads) | Spike-R3 (S0) | MEDIUM | **Length-prefix framing** (pre-committed) — also required by R2; collapses R2+R3. |
| **R4 — rust_tcl REPL persistence via re-eval** (replay of past wire calls) | Spike-R4 (S0) | MED-LOW | Exclude wire verbs from the replay buffer; or a small upstream persistent-`Vm`. |
| **R-snap — framebuffer reachable from `inject.was`** | Spike-R-snap (S0) | MEDIUM | Export the `fb`/Snapshot pointer (`.globl`) — a one-line library change. |
| **R5 — `was --emit-verbs` parser change** | S5 (reuses `parse_proc`) | LOW | Keep the hand-written `verbTbl` from S1. |
| **R6 — verbs must register before `eval`** (immutable `&Registry` borrow) | confirmed in code (`vm.rs:23`), built S5 | LOW | One-line catch-all at `vm.rs:328`; explicit `call` always works. |

## Status

- ✅ **Sprint 0** — spikes all **PASS** (findings below): R1 resume works · R3 length-prefix · R4 persistence · R-snap `fb` reachable.
- ⬜ Sprint 1 — walking skeleton
- ⬜ Sprint 2 — pipe + baked scripts + persistence + dev-gate
- ⬜ Sprint 3 — full async round-trip *(→ CLI CI-capable)*
- ⬜ **GATE** — happy-with-CLI criteria
- ⬜ Sprint 4 — float
- ⬜ Sprint 5 — discovery + bare-verb sugar
- ⬜ Sprint 6 — dashboard
- ⬜ Sprint 7a — exception channel · ⬜ Sprint 7b — studio graft

### Sprint 0 — findings (all PASS, locked decisions)

- **R1 — VEH catch-and-resume: PASS, resume works.** A vectored handler catches a hardware
  #AV inside a real `frame` proc and resumes via a `CONTEXT.Rip/Rsp/Rbp` rewrite +
  `EXCEPTION_CONTINUE_EXECUTION`; it re-arms and survives repeated faults, **with no
  `.pdata`/unwind tables**. → **Sprint 7a builds the live exception channel; report-then-
  respawn is dropped.** *Trap baked into the build:* a bare `[0]` encodes RIP-relative (never
  faults) — null-deref through a register. CONTEXT offsets: `Rsp 0x98`, `Rbp 0xA0`, `Rip 0xF8`.
- **R3 — framing: PASS.** `[u32-LE length][payload]` on a **byte-mode** pipe + a read-exact
  reassembly loop stitched a fragmented 8 315-byte frame (6 reads) byte-exact. → Use it over
  `PIPE_TYPE_MESSAGE` (also socket-portable); cap the decoded length against a hostile prefix.
- **R4 — REPL persistence: PASS, with a caveat.** Accumulate-and-re-eval persists vars/procs,
  but **re-running the buffer double-fires side-effecting wire verbs**. → script mode is a
  single `eval` (no issue); the interactive REPL gets a small **persistent-`Vm`** entry in
  `rust-tcl` (we own the crate) rather than replay-with-exclusion.
- **R-snap — reachability: PASS.** Stream the **indexed `fb`** (64 000 B, `.globl` already)
  + `palette`/`linePal` and resolve RGB tool-side (¼ the BGRX bandwidth, no library change).
  The resolved `pbuf` is private (`Canvas$pbuf`); add `.globl pbuf` only if post-resolve
  pixels are ever wanted.

Key targets: asm under `library/nanotcl/` (`shadow.was`, `call.was`, `lexreg.was`,
`lexval.was`, `verbs.was`, `repl.was`, `inject.was`, `num.was`, shell `nanotcl.was`);
reuse `library/audio/sound/sound_play.was` (`CreateThread`+`xchg` spinlock) and
`abc_scan.was` (cursor/lexer); hook `library/harness.was` (`GameFrame`/`HWndProc`, add
`cmp edx,0x4A`); `was --emit-verbs` in `crates/was/src/lib.rs` (`parse_proc`); Rust crate
`crates/nanotcl/` over `rust-tcl` (`e:\doccrate\crates\rust-tcl`); studio graft at
`script_registry()` in `crates/studio/src/bin/studio.rs`.
