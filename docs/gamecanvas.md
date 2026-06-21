# GamesCanvas (RASM) — design & status

Our own design doc for the 2D canvas, hand-written in x86-64 via the WRASM
toolchain. Derived from the language-agnostic spec (`E:\NewModula2\GamesCanvas.md`)
but written against *our* primitives, conventions, and roadmap. Companion:
[gameaudio.md](gameaudio.md). Goal: **Canvas + audio + tunes + input = 2D games.**

## Status

| Area | State | Where |
|---|---|---|
| Index framebuffer + 256 palette | ✅ | `library/canvas.was` |
| GDI present (StretchDIBits, 3× nearest) | ✅ | `present` |
| `Cls` / `FillRect` | ✅ | `FillRect` clips all 4 edges |
| `Pset` / `Pget`, `HLine` / `VLine` | ✅ | |
| `Line` (Bresenham) / `Circle` (midpoint) / `Disc` | ✅ | verified pixel-exact |
| `Rect` (outline) | ✅ | |
| `Text` (5×7 font) | ✅ | `library/gc_font.inc` |
| Palette cycling | ✅ | `CyclePalette` |
| **Per-line palette LUT** (0..15 per scanline) | ✅ | `resolve_present`, `SetLineColour` |
| Global "regular 240" (indices 16..255) | ✅ | `palette` |
| **Inter/intra-buffer blit** | ✅ | `library/blit.was` |
| Sprites (text-authored + keyed blit) | ✅ | `library/sprite.was` |
| **Per-sprite palette** (own CLUT, composited) | ⬜ next | |
| **Overscan world + smooth scroll** | ⬜ next | |
| **Double buffering** | ⬜ (built on blit) | |
| Introspection (timestamped snapshot) | ✅ | `library/introspect.was` |
| Input (keyboard/mouse state) | ⬜ | |
| GPU profile (D3D11) | ⬜ | builds on `examples/mandel_gpu.was` |

## Model

A **320×200 8-bit *index* framebuffer** (`fb`) + a **256-entry palette** of
`0x00RRGGBB` (little-endian → B,G,R,0 = BGRX, no swizzle for a BI_RGB DIB).
Primitives write palette *indices*; `present` resolves `fb` through `palette` into
a BGRX scratch (`pbuf`) and `StretchDIBits`-es it to the window with a 3× nearest
upscale (the chunky DOS/Amiga look). Default palette = the canonical CGA/EGA 16.

Everything is a contract-checked `proc` (declared `uses`/`in`, opt-in `frame`), so
the WRASM checks guard the whole library — they already caught a label collision
and undeclared inputs while it was being built.

### Calling convention (our primitives)

Integer args in `ecx, edx, r8d, r9d` then `r10b`, matching the demo:

| proc | args |
|---|---|
| `Cls` | `cl`=index |
| `Pset` | `ecx`=x `edx`=y `r8b`=index |
| `HLine`/`VLine` | `ecx`=x `edx`=y `r8d`=len `r9b`=index |
| `FillRect` | `ecx`=x `edx`=y `r8d`=w `r9d`=h `r10b`=index |
| `Line` | `ecx`=x0 `edx`=y0 `r8d`=x1 `r9d`=y1 `r10b`=index |
| `Circle`/`Disc` | `ecx`=cx `edx`=cy `r8d`=r `r9b`=index |
| `Text` | `ecx`=x `edx`=y `r8b`=index `r9`=asciiz |

All clip to the framebuffer (negative coords rejected via unsigned compare).

## Palette architecture — thousands of colours on a low-depth display

A true-colour (24-bit) screen driven by an indexed framebuffer through **many
independent LUTs**. Every LUT entry is a real **RGB** colour (`0x00RRGGBB` — *no
alpha*); **index 0 is 100% transparent** everywhere (the only transparency, binary).

- **Per-line LUT** — background indices **0..15** resolve through *that scanline's*
  own 16-colour LUT (`linePal[row][0..15]`). One index → a different colour on every
  line: gradients, raster bars, split palettes. ✅ (`resolve_present`,
  `SeedLinePalette`/`SetLineColour`).
- **The regular 240** — background indices **16..255** resolve through the single
  global `palette`. The fixed, shared colours.
- **Per-sprite LUT** — each sprite has its own 16-colour LUT, **independent** of the
  line and global LUTs. A sprite pixel is `4-bit → that sprite's LUT → RGB`,
  composited over the background (index 0 skipped, else opaque). ⬜ next.

Because the LUTs are independent and each holds a real colour, the simultaneous
distinct-colour count *multiplies*: `240 + 16·(lines) + 16·(sprites)`. A 640-line
screen with 128 sprites → 240 + 16·640 + 16·128 = **12,528** unique colours in a
single frame, all from 4-/8-bit indices.

Pipeline: `resolve_present` builds the background into the 32-bit `pbuf` (per-line
LUT for 0..15, global for 16..255); sprites then composite over `pbuf` through their
own LUTs (0 transparent, else opaque RGB); `pbuf` is blitted. One `resolve_present`
keeps the screen and the introspect snapshot identical.

Status of sprites vs this model: CPU sprites today are **blitted** into `fb`, so they
share the background's line/global palette (M2's CPU profile does the same). A
**per-sprite-palette compositing** path (`BlitMap`-style remap, or a sprite layer
resolved at present) is the next step — that's what makes a sprite carry its own
colours and palette-swap.

## Overscan & smooth scrolling (design)

The index buffer is **larger than the display** (e.g. an overscan world up to
640×400, or a wide 1280-class world) and the display shows a **window** into it at a
start position `(scrollX, scrollY)`. Smooth scrolling = move the start position; no
pixels are copied. `present` reads `fb[(scrollY+sy)*worldW + (scrollX+sx)]` for each
display pixel, and the per-line LUT is indexed by the display row. Display modes:
320×200 and 640×400, integer-upscaled to the window. ⬜ next-phase work (today the
buffer == the 320×200 display).

## Double buffering (design)

The GDI present is already *atomic* (one `StretchDIBits`), so a full-frame
"draw into `fb`, then present" is flicker-free today. Double buffering earns its
keep for **partial redraw** — keep an expensive static background and only repaint
what moves, without rebuilding the whole frame each tick.

Plan — two index buffers and a draw pointer:

```
fbA   BYTE 64000 dup(0)      ; buffer A
fbB   BYTE 64000 dup(0)      ; buffer B
drawPtr  QWORD ?             ; current back buffer (drawn into)
showPtr  QWORD ?             ; current front buffer (presented)
```

- Primitives take their target from `drawPtr` instead of a hard-wired `fb` (so the
  pixel API gains an implicit "current draw buffer"; `SetDrawBuffer(ptr)` sets it).
- `Flip` swaps `drawPtr`/`showPtr` and presents `showPtr`.
- Static background: render once into both; thereafter restore-blit only the dirty
  rects (see below), draw movers, `Flip`.

This rides on the blit primitive — restore = inter-buffer blit of the saved
background rect; that's the cheap "erase".

## Inter/intra-buffer blit (implemented — `library/blit.was`)

One primitive underlies sprites, scrolling, background save/restore, and the
double-buffer flip. A **buffer** is any index bitmap (the framebuffer, an offscreen
page, a sprite sheet, a saved-background scratch). The destination is a *settable
surface* (defaults to the framebuffer); the source rect starts at the `src`
pointer (point it into a sheet + pass that sheet's stride for a sub-rect).

```
DestFramebuffer                                   dest = the canvas fb (320x200)
SetDest(rcx=base, edx=stride, r8d=w, r9d=h)       dest = a custom surface
SetKey(ecx=index)                                 transparent index (default 255)
Blit   (rcx=src, edx=srcStride, r8d=w, r9d=h, r10d=dstX, r11d=dstY)   opaque
BlitKey(rcx=src, edx=srcStride, r8d=w, r9d=h, r10d=dstX, r11d=dstY)   skip == key
```

- **Intra-buffer** (`src == dst`): scrolling and on-canvas moves. The copy
  direction is picked by overlap (`cmp dstPtr,srcPtr`) so an overlapping region
  isn't corrupted mid-copy (assumes a shared stride — always true within a buffer).
- **Inter-buffer** (`src != dst`): sprite sheet → framebuffer; framebuffer → saved
  background (and back, to erase); back page → front (the `Flip` fast path).
- **Clipping**: clips the destination rect to the dst surface, adjusting the src
  origin + w/h in lockstep (the `HLine` edge clip, in 2D), overflow-safe.
- **Row engine**: a tight per-pixel copy with the opaque/key decision hoisted to
  registers outside the inner loop. (A `rep movsb` fast path for the opaque,
  unclipped, non-overlapping case is a future optimization.)

### Sprites fall out of this

A sprite is a small index buffer authored as text (`"....22.../..2332.."` → bytes,
`'.'` = the transparent key 255). `DefineSprite` parses text → an index bitmap +
w/h; `DrawSprite(id, x, y)` = `BlitKey` from the sprite into the current draw
buffer. `BlitFlip`/`BlitScale` are row-engine variants (mirror, nearest stretch).

## Profiles

- **CPU** (now): index `fb` → palette resolve → `StretchDIBits`. Simple, exact,
  portable. The reference implementation.
- **GPU** (next): upload `fb` as an R8 index texture + the palette as a 256×1 LUT;
  a pixel shader does the lookup; sprites are a second textured pass. Builds on the
  D3D11 plumbing in `examples/mandel_gpu.was` (shader via `.ASCIISTRING` +
  `D3DCompile`, the COM macros). Same authoring API, two backends.

## Input

`library/input.was` — two layers. **Raw**: `InputKey` (from `WndProc`) fills a
256-key state + edges (`KeyDown`/`KeyHit`), plus mouse and a `joyGetPosEx` joystick.
**Actions**: 8 device-independent predefined states — `LEFT RIGHT UP DOWN FIRE PAUSE
RESTART QUIT` — each mapped from *both* keyboard (arrows+WASD, Space/Ctrl, P, R, Esc)
and joystick (axes + buttons), queried with `Action`/`ActionHit`. `InputPoll` (once
a frame, the harness's job) reads the stick and recomputes the actions + edges.

## Introspection — and headless play

`library/introspect.was` — `call Snapshot` resolves the current `pbuf` and writes
`snap_<NNNN>.bmp` (sequence-numbered, so a rapid series is an ordered, collision-free
**filmstrip** — *sample* a run, don't just snap one frame). Window capture is
unreliable here; this is the reliable way to *see* what's drawn (→ PNG via
System.Drawing).

Crucially, the actions are **simulatable**: `SimAction(a, down)` forces a core state,
so a headless self-test can *play* the game with nobody at the keyboard — inject an
action, step `game_frame`, sample. The demo drives the hero right via `SimAction` and
films 5 frames of the motion. This is how gameplay gets verified without a human.

## Toolkit scope — the "20 games" test

The boundary: *what would you not want to rewrite across 20 retro games?* That's the
library. What expresses **this** game is yours. The library owns the **machine**
(identical every game); the author owns the **game** (unique every game). Rich
primitives, no imposed architecture — no mandatory ECS, scene graph, or scripting.

**Library — write once, reuse every game:**
- **Harness** — window + message pump + fixed-step loop + present, calling the
  author's `update(dt)`/`render()`. (Today the demo hand-writes `WndProc`+timer+loop.)
- **Input** — `KeyDown`/`KeyHit` (edge) + mouse, filled by the harness.
- Canvas + LUT palettes + primitives + `present` ✅; text/font ✅.
- **Sprites** — define + per-sprite LUT ✅; frames/animation, flip, scale, AABB collision.
- **Tilemap** — tiled, scrolling background over the overscan buffer.
- **Audio** — `PlaySfx(id)` / `PlayMusic(tune)`.
- **LUT effects** — fade, flash, colour-cycle, line-gradient (the colour magic, one-liners).
- **Util** — deterministic RNG + sin/cos tables.

**Author — fresh each game:** the art, sound, levels; the game state / rules / AI /
scoring in their own structures; the bodies of `update(dt)` and `render()`. The
sprite-instance pool / collision / physics are **opt-in**, never load-bearing.

Success test: game #20 is *"new art + a tilemap + a few dozen lines of
update/render"* because the machine was written for game #1.

## Roadmap (logical bits)

Ordered by the "20 games" test — most-needed, most-boilerplate first:

1. **Harness** — `GameRun(init, update, render)`: own the window, message pump,
   fixed-step timing, input, and the resolve→composite→blit present. *Biggest cut in
   per-game boilerplate.*
2. **Input** — key-state + edge + mouse (filled by the harness).
3. **Sprite animation** — `AddFrame` + a frame index/timer; then flip/scale + AABB.
4. **Tilemap** — a tileset + map → scrolling background (with overscan + scroll).
5. **LUT-effect helpers** — fade/flash/cycle/gradient; RNG + trig tables.
6. **Audio** — the SFX synth + `PlaySfx`/`PlayMusic` ([gameaudio.md](gameaudio.md)).
7. **GPU profile** — index texture + per-line/per-sprite LUT shader + sprite pass.

Then it's a playable engine that gets out of the author's way.
