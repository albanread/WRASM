# Studio introspect view — design

A graphical **control panel + dashboard** inside the studio IDE that drives and inspects a
*running* WRASM game over the nano-TCL channel ([nano-tcl.md](nano-tcl.md) Tier-2). It is the
human face of the Tier-1 register-machine probe: the game ships raw register bytes and events,
the studio turns them into a live, labelled, clickable debugger.

> The Tier-1 side lives **in the game** (`library/tcl/*`, wired into the game's GameInit/GameStep
> — see `projects/galaxigans` for the reference wiring). This doc is the **studio (Tier-2) UI**.

---

## 1. Where it lives — the right pane, code stays visible

The panel **replaces the assistant/help view** in the studio's right pane, toggled with it by a
tab (`Assistant | Introspect`). It does **not** take a third column: the editor — your program
code — stays in place on the left. That adjacency is the point of the layout:

- When a **breakpoint trips, the editor scrolls to and highlights the paused source line**, so
  the watch values (right) and the instruction that produced them (left) describe one moment.
- Reuses the existing splitter/`collapse`/`expand` model; no new window geometry.

(Mockup: editor left with the paused `je cbh_active` highlighted; introspect dashboard right.)

---

## 2. The control surface — run, step, run-until

The top band is playback control over the game's own 60 fps loop (it advances via the frame-sync
channel; the studio never drives frames itself):

- **Transport** — Run / Pause / Step-one / Stop, with a live `frame N · fps` readout.
- **Run to frame N** — type a target; the game free-runs to that frame and pauses.
- **Step N frames** — advance a fixed count.
- **Conditional breakpoints** — `watch op value` rows (`bossState == BEAM`, `lives < 3`,
  `score >= 1000`). The studio evaluates them **locally** against each frame's state ping and
  pauses the instant one trips. Each is individually armable; a tripped one shows the frame it
  hit (`hit @ 1281`). This is "run until a condition" without scripting it.

---

## 3. The watch panel — the human layer

The game exposes numbered register slots (`TclReg slot, value`). The studio maps them to
**named, enum-decoded values** so the table reads `bossState = BEAM`, not `slot1 = 5`:

- **Manifest** — a tiny per-game table `{slot → name, enum {0:IDLE 5:BEAM …}, format}`. Source
  options: the game registers it once over the channel (`watch 1 bossState enum {…}`), or the
  build emits a sidecar the studio reads on connect.
- **Change feedback** — a value that moved this frame flags with a delta (`▲500`) or `↻`; a
  per-watch mini-history (sparkline) shows a trend across recent frames.

---

## 4. The live preview

A thumbnail of the actual frame the numbers describe: the studio resolves the game's streamed
indexed framebuffer + palette (`fb`/`palette`/`linePal` are already `.globl`) to RGB and draws
it. Paused, it is the still that goes with the watch values; running, it is a remote viewport.

---

## 5. The event log

The demux'd channel stream, newest last, colour-coded and frame-stamped:

- `›` commands you sent · `‹ OK` replies · `out:` the game's `puts` · `⏸ break` pauses ·
  red `exc:` fault dumps (faulting address + captured registers — the channel survives a bad
  poke, so a crash shows here instead of killing the session).
- The **ordering is explicit** (command sent @ frame, effect visible @ frame), which removes the
  guesswork about when a poke takes effect relative to a frame.

---

## 6. The command bar

Type any exported verb with **autocomplete from the game's verb table** (`bossarm`, `hitboss`,
`Pset rcx=…`). Two affordances make exploration durable:

- **Verb buttons** — frequently-used verbs surface as one-click buttons.
- **Save session as test** — the sequence of verbs + the breakpoints/asserts you ran exports to a
  `.tcl` regression test (exactly how `projects/galaxigans/tests/capture_boss.tcl` would be born),
  runnable headless by the `nanotcl` CLI for CI.

---

## 7. The wire & the connection

The panel is the Tier-2 client, factored into the studio's existing `script_registry()` (nano-tcl
§18) — one set of wire verbs, two front-ends (the standalone `nanotcl` CLI and this panel). The
studio's 60 fps render loop *is* the dashboard renderer (nano-tcl §15), so live state, the preview,
and the log update every frame for free. Connection: `connect` (attach to a running dev build),
or `spawn` (launch one and attach). Drawn in Direct2D like the other panes.

---

## 8. Tier-1 additions the GUI wants (in the game)

Some panel features need small in-game support beyond what `library/tcl` has today:

- **Read-by-name / ad-hoc watch** — a generic `Peek <label>`-style read so the studio can watch
  an arbitrary global *without a game rebuild* (today each watch is a hand-added `TclReg` slot).
- **Proc/label breakpoints with a hit count** — arm a break on an exported proc; the executor
  pauses when it is entered, and reports a **hit count** (0 = never entered). See §9 (lessons) for
  why this matters most.
- **`fb` frame events** — opt-in streaming of the indexed framebuffer for the live preview.

---

## 9. What the first live session taught us

These priorities are not hypothetical — they come from an actual session: debugging the
capture-boss rescue, which turned out to be **dead code** (`CheckBossHit` had been dropped from
GameStep by a stray `git checkout`). Done over the raw `nanotcl` CLI, the friction was concrete,
and each point maps to a panel feature:

- **The bug was a missing call, and the watch table only showed the symptom.** The bullet sat
  inside the boss box yet `abducting` never changed — I had to *infer* "the proc isn't running",
  then confirm by reading the source. → **Proc breakpoints with a hit count, set in the editor
  gutter.** Click the proc/line, run, and a `0 hits` counter says "never entered" outright. This
  is the single highest-value feature the session revealed — `CheckBossHit · 0 hits` would have
  named the bug in one run instead of three rounds of inference.

- **Every new thing to watch meant a rebuild.** Seeing the bullet's x/y took new `TclReg` slots +
  a ~10 s rebuild + re-run — twice. → **Ad-hoc watches by name** (`bullets[0].y`) read live via a
  Tier-1 `Peek`, no rebuild. The rebuild loop was the biggest time sink of the session.

- **I was scripting exploration.** Write `.tcl` → run all → read → edit → re-run, several cycles,
  for a handful of values at a handful of frames. → **The watch table + command bar ARE the REPL**:
  poke `bossarm`, watch the table move, poke `hitboss`, watch it move. The `.tcl` is the *export*
  of a good session (a regression test), not the medium of exploration.

- **Frame-sync timing was guesswork.** "Does my poke land before or after this frame's logic? How
  many `regs` to settle?" — found by trial. → **Explicit single-step + an ordered, frame-stamped
  log** make "ran @ N, visible @ N+1" legible. (Implementation note: publish the state ping at
  END of frame so "step, then read" shows the frame you just ran.)

- **Connecting was a five-step dance** — build, background-launch, sleep-hack, `nanotcl`,
  `taskkill`. → **One-click Build & launch** is the primary connect affordance, with a Stop.

- **The payoff justifies the test machinery.** The harness caught a regression the BMP filmstrip
  *structurally cannot* — dead code renders identically on screen. → the case for **save-as-test +
  an in-panel green/red runner**, so the assertion outlives the session.

**The synthesis** ties back to §1: the session *validates the layout choice*. The most valuable
control — a breakpoint with a hit count — belongs in the **editor gutter**, which is only on
screen because the panel replaced the help view rather than the code. Debugging wants the code and
the dashboard together; the watch table tells you *what* is wrong, the gutter breakpoint tells you
*where* it isn't running.

## 10. Implementation phasing (studio side)

1. **Connect + watch** — `connect`/`spawn`, the frame-sync poll, the named watch table (manifest).
2. **Control** — transport + run-to-frame/step + local conditional breakpoints + the source-line
   link.
3. **Log + command bar** — the demux'd event log, verb autocomplete, save-as-test.
4. **Preview + faults** — the `fb` thumbnail and the `exc:` fault surface.

Each phase is independently useful; phase 1 alone replaces the manual CLI loop.
