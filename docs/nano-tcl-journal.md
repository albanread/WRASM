# nano-TCL field journal — building & testing "Jewels"

A first-person log of using nano-TCL to build a real game (RASM Jewels, a
Columns clone) and test it end to end — written for the team, warts and all.
The point isn't to sell the tool; it's to record what it actually felt like to
lean on it, what paid off, and where it fought me.

## The setup

I wrote the game (`projects/jewels/`, ~5 files of hand x86-64 on the CPU
GamesCanvas library), then wired it for nano-TCL: a dev build opens the pipe,
the game registers its own verbs (`setpiece`, `left`, `right`, `drop`, `step`,
`newgame`, later `probe`) and publishes state via `TclReg` into the per-frame
ping (`0=score 1=state 2=pieceCol 3=pieceRow 4=cleared 5=probe 6=level`). The
test is a `.tcl` file run by the `nanotcl` driver:

```
projects/jewels/jewels.exe                    # start the game
nanotcl projects/jewels/tests/level1.tcl      # exit 0 = all asserts passed
```

## What worked — and genuinely changed how I built

**Driving the game into exact states.** The single biggest win. Match-3 logic is
all about specific board configurations, and reaching them by "playing" is slow
and random. With `setpiece rcx=9 rdx=10 r8=12` + `left` + `drop` I built the
precise scenario I wanted — blue/green/red into columns 0,1,2 — and asserted the
*chain*: reds clear, greens collapse and match, blues collapse and match, score
exactly 90. I could never set that up reliably by hand, and a human play-test
would never hit it on demand.

**Deterministic state, not eyeballs.** `regs` reads the published slots, so the
assertions are facts: `score == 90`, `clears == 9`, `state == 2`. No OCR, no
"looks about right." When I broke something, the failure told me the number it
got. That's a real regression net — I can refactor the match scanner and the
test will catch a wrong count immediately.

**The moment it proved its worth.** My first chain test *looked* broken — the
snapshot showed score 0 and the starting piece. I almost went bug-hunting in the
match logic. Instead I read the actual state through the ping: `score=30,
clears=3`. The logic was *fine*; the snapshot was stale (see below). The tool
told me the truth the picture was hiding. That single read saved a wild goose
chase.

**Rendered-pixel assertions beat snapshots.** Late in the session I added a
`probe x y` verb (it just calls the library `Pget` and stashes the result in a
slot). Now the test asserts the *drawn* output: drop a red jewel into a cell,
`probe` its centre pixel, assert `== 12` (red). That's the thing a snapshot is
supposed to check — except no human has to look, and it's in the CI exit code.
Snapshots tell you *something* rendered; `probe` tells you *exactly what*.

**Generic, not game-specific.** I deliberately did *not* teach the driver about
jewels. I added two primitives — `send "<verb args>"` (forward any raw line) and
`regs` (one frame-sync snapshot → 16 registers as a list) — and the whole test is
written in those. The same two verbs will test the next game with zero changes
to the tool. That felt like the right layer to generalize at.

**The two-tier split earns its keep.** All the loops, vars, and asserts run
*locally* in `rust_tcl`; only the substituted `Verb reg=val` lines cross the
wire. The in-process executor stays dumb. I never once wished the heavy logic
lived inside the game.

## What bit me (the honest part)

**Frame ordering is invisible until it isn't.** Three separate timing gotchas,
all from the same root — *when* in the frame each thing runs:
- `Snapshot` runs inside `TclPoll`, which is *before* `DrawBoard` in the step. So
  a `Snapshot` in the same batch as my moves captured the *previous* frame's
  framebuffer. Fix: send `Snapshot` in a later frame. Obvious in hindsight,
  baffling for ten minutes.
- The ping reads the shadow registers that `ExposeState` writes at the *end* of
  the frame, so a value is always one publish behind the action that set it.
- `probe` reads `fb`, which is only correct *after* `DrawBoard` — so the probe
  has to land a frame after the move that should have drawn it.
  None of this is wrong, but nothing told me the order. I had to derive
  `InputPoll → TclPoll → AdvanceFrame(ExposeState) → DrawBoard` by reasoning, and
  every test that reads state has a `regs` or two of slack to absorb the lag.

**`SimAction` latency.** Injected input is merged at the *next* `InputPoll`, and
the force-latch persists until cleared. So testing the real keyboard path (FIRE
to restart) is `SimAction rcx=4 rdx=1` → advance two frames → `SimAction rcx=4
rdx=0`. Correct and even principled, but I had to *know* it; my first attempt
asserted too early and read the old state.

**The Tier-1 parser is deliberately tiny — and you feel the edges.** No negative
numbers: `move rcx=-1` silently parses as `0`, so the piece didn't move and I
couldn't tell why. I switched to nullary `left`/`right` verbs. Fine, but it's a
trap with no diagnostic — the parser just stops at the `-`.

**`rust_tcl` is a subset.** `for` isn't implemented (I got "expected expression"
and assumed my syntax was wrong); command substitution inside `expr {}` —
`expr {[lindex $r 1] == 2}` — also fails, you must pull it into a `set` first.
Both are reasonable for a minimal TCL, but I burned a couple of cycles before I
pattern-matched to "extract to a var, unroll the loop."

**Reading a verb's return is a workaround.** `nanoCall` captures a verb's return
in shadow slot 0 — but `ExposeState` overwrites slot 0 with the score every
frame, so I couldn't read `Pget`'s result directly. That's *why* `probe` exists:
a game-side verb that stashes the return somewhere `ExposeState` then publishes.
It works, but "read what a verb returned" shouldn't need a bespoke verb per game.

## What it taught me about the design

- **`TclReg` + the ping is the contract.** The game decides what to expose; the
  tester reads it by slot. Clean, but undocumented per game — the test has a
  magic-number comment (`0=score 1=state …`) that has to stay in sync by hand.
  A game declaring its slot names would close that.
- **Generalize at the primitive, not the verb.** `send`/`regs` were the right
  call; resisting the urge to bake jewels verbs into the driver kept it reusable.
- **`probe` is the snapshot-killer I didn't plan.** Pixel read-back turns "a human
  looks at a PNG" into "CI asserts index 12 at (153,177)." That's the real answer
  to *is TCL better than snapshots* — not "instead of," but "the same check,
  automated."

## Snapshots vs. TCL — the verdict from the trenches

TCL state assertions win decisively for **logic** (exact, deterministic, drives
to states, CI exit code) and, with `probe`, now win for **rendering** too. The
only thing snapshots still do better is *exploratory* looking — when I don't yet
know what to assert and just want to *see* it (the title screen, "does the
colour palette read right"). I used snapshots exactly there: to eyeball new
visuals once, then pin the important pixels with `probe`.

## Wishlist (concrete, ranked)

1. **Negative integers in the Tier-1 parser** (`rcx=-1`) with an error on a bad
   number instead of a silent `0`. The single sharpest edge.
2. **A first-class "read a verb's return"** — e.g. `call Pget rcx=… rdx=…` that
   surfaces shadow slot 0 *before* the next `ExposeState`, so games don't each
   need a `probe`.
3. **Document the frame order** (InputPoll → TclPoll → step/ExposeState →
   DrawBoard) somewhere a tester will read it. Half my friction was not knowing
   it.
4. **A `waitframes N` driver helper** so a test can advance N frames without N
   `regs` round-trips (and without the intro/free churn each time).
5. **Let a game name its ping slots** (a `slots score state …` registration) so
   `regs` can return a dict and tests stop hard-coding indices.
6. **`rust_tcl`: `for`/`incr`**, or at least a one-line note that they're absent.

Net: I'd reach for it again without hesitation. It made a fiddly,
state-dependent game *testable* in a way snapshots never could, and the rough
edges are all "I didn't know the timing," not "the model is wrong."
