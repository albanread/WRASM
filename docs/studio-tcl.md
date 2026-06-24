# Studio TCL scripting

The **studio** IDE embeds a small **TCL** interpreter (Tool Command Language) so the
editor can be driven headlessly — for UI tests, reproducible screenshots, and demos
— without a desktop. A script gets the full TCL core (`set` / `if` / `expr` /
`foreach` / `proc` / …) plus a set of **verbs** that act on a windowless instance of
the IDE.

```sh
studio --script tour.tcl       # run a TCL UI script, then exit
studio --exec "open game.was; pump; screenshot shot.png"   # inline script
studio --shot [dir]            # render one frame to a PNG and exit (no script)
```

A non-zero exit on any `assert*` failure means **a script doubles as a UI test**.

## Conventions

- Coordinates are in **DIPs**; the default headless viewport is **1100×720** (change
  it with `size`).
- `expr` here is **integer-only** — use literal numbers for coordinates.
- Use **forward slashes** in paths (TCL treats `\` as an escape).
- Editor shortcuts the verbs reach: `Ctrl+F` find-in-file, `Ctrl+Shift+F` Windows-API
  search, `Ctrl+G` go-to-label palette.

## Verbs

### Input

| verb | does |
|---|---|
| `type "text"` | type characters into the editor |
| `key NAME` | one key: `Enter Backspace Delete Tab Escape Left Right Up Down Home End PageUp PageDown`, or a letter; prefix `Ctrl+` / `Shift+` / `Alt+` (e.g. `key Ctrl+S`) |
| `caret ROW COL` | place the caret (0-based row/col) |

### Pointer

| verb | does |
|---|---|
| `move X Y` | move the mouse (hover — e.g. lights a splitter) |
| `click X Y` | press + release at a point |
| `drag X1 Y1 X2 Y2` | press, move, release (resize a splitter, select, …) |

### Panes & window

| verb | does |
|---|---|
| `splitter col FRAC` | set the editor\|assistant split (0.05–0.95) |
| `splitter row PX` | set the output-pane height (DIPs) |
| `collapse PANE` / `expand PANE` | hide / reveal a pane (`PANE` = `assistant` \| `output`) |
| `open FILE` | load a `.was` file into the editor |
| `new` | start an empty buffer |
| `size W H` | set the viewport size (pixels) |
| `pump ?ms?` | settle async replies (caret cards, live checks) before a screenshot — default 400 ms |
| `screenshot PATH.png` | render the current frame to a PNG file |

### Read-back (for assertions)

Each returns a string you can compare. Editor: `text`, `line N`, `linecount`,
`caret-row`, `caret-col`, `selection`. Status / cards: `notice`, `card-md`. Layout:
`split-frac`, `output-h`, `assistant-open` (1/0), `output-open` (1/0), `hover-split`
(`col`\|`row`\|`none`). Find-in-file: `fbar-count`, `fbar-needle`. Go-to-label
palette: `find-active` (1/0), `find-query`, `find-count`, `find-selected`.

### Assertions

| verb | does |
|---|---|
| `assert COND ?msg?` | fail (exit 1) unless `COND` is true / non-zero |
| `assert-eq GOT WANT ?msg?` | fail unless `GOT` equals `WANT` |

## Example

```tcl
# tour.tcl — type a program, check the live diagnostics, screenshot it.
new
size 1100 720
type "proc main\n  invoke ExitProcess, 0\nendproc\n"
caret 0 0
pump                                  ; let the live check + caret card arrive
assert-eq [linecount] 4 "expected 4 lines"
expand assistant                      ; show the knowledge card pane
screenshot out/tour.png
```

A worked script ships at `crates/studio/scripts/ui_demo.tcl`. The same verb list is
available from `studio --help`.

> **Note on `was`.** The CLI assembler (`was`) is deliberately a one-shot batch tool
> (source → `.exe`/`.obj`) and does **not** embed TCL — there is no stateful session
> to drive, and a shell already sequences multiple `was` invocations with assertions.
> TCL lives in `studio`, where there's a live UI worth scripting.
