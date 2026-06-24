# RASM Jewels — a Columns-style puzzle (level 1)

A small match-and-drop puzzle, hand-written in x86-64 (WRASM dialect) on the CPU
GamesCanvas library, wired for **nano-TCL** so it can be driven and tested from a
script.

## Play

A triple of jewels falls into a 6×12 well. Line up **3 or more of one colour**
horizontally or vertically to clear them; the stack collapses under gravity and
**chains** score more. Game over when the spawn column fills to the top.

| key | action |
|-----|--------|
| ◀ / ▶ | move the falling column left / right |
| ▲ | cycle the triple's three colours |
| ▼ | soft-drop (fall faster) |

Scoring: 10 per cleared jewel (a 3-row chain that clears 9 jewels = 90).

## Build & run

```
was projects/jewels/jewels.was -o projects/jewels/jewels.exe
projects/jewels/jewels.exe
```

To ship a **release** build (no live channel), delete the `NANOTCL_LIVE equ 1`
line in `jewels.was` — the whole nano-TCL pipe compiles out; the game is
unchanged otherwise.

## Files

| file | role |
|------|------|
| `jewels.was` | shell: nano-TCL flags, the library includes, the project includes |
| `jewels_data.inc` | board geometry constants + game state |
| `jewels_logic.was` | rng, spawn, fall/lock, match scan, clear, gravity-collapse, chains, input, app verbs |
| `jewels_draw.was` | the well + border, the jewels (discs), the falling triple, the score |
| `jewels_main.was` | `main` + the three harness callbacks + nano-TCL wiring |
| `tests/level1.tcl` | the nano-TCL test (run by the `nanotcl` tool) |

## Test with nano-TCL

The dev build opens `\\.\pipe\nanotcl`. The game registers its own verbs for
deterministic testing — `setpiece rcx=c0 rdx=c1 r8=c2`, `left`, `right`, `drop`,
`step`, `newgame` — and publishes state into the introspector ping:
`0=score 1=state 2=pieceCol 3=pieceRow 4=cleared`.

```
projects/jewels/jewels.exe                    # 1. start the game
nanotcl projects/jewels/tests/level1.tcl      # 2. run the test  (exit 0 = pass)
```

The tests drive the game with `send "<verb args>"` and read published state with
`regs` (and rendered pixels with `probe`). Both `send` and `regs` are generic
`nanotcl` primitives — they work against any nano-TCL game, not just this one.

### Example scripts (`tests/`)

| script | what it demonstrates |
|--------|----------------------|
| `level1.tcl` | the level-1 rules: vertical = 30, horizontal + chain = 90, no false match, game over, FIRE-to-restart (real input path), and a rendered-pixel probe |
| `moves.tcl` | input + a published slot — the falling column clamps at both walls (`pieceCol`) |
| `probe_board.tcl` | rendered-board assertions — `probe` each drawn jewel's centre pixel and the black gap (beats an eyeballed snapshot) |
| `demo.tcl` | no asserts — drives a scripted game and narrates the score (the "watch it play" mode) |

Run any of them while the game is up: `nanotcl projects/jewels/tests/<name>.tcl`
(exit 0 = all asserts passed). See `docs/nano-tcl-journal.md` for a field log of
building and testing this game with nano-TCL.
