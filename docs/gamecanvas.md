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
| **Inter/intra-buffer blit** | ✅ | `library/blit.was` |
| **Double buffering** | ⬜ design below (built on blit) | |
| Sprites (text-authored + keyed blit) | ⬜ (built on blit) | |
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

## Introspection

`library/introspect.was` — `call Snapshot` resolves the framebuffer and writes
`snap_<ticks>.bmp`, a timestamped self-portrait. Window capture is unreliable
here; this is the reliable way to *see* what the canvas drew (convert to PNG with
System.Drawing). A core development feature, included early.

## Roadmap (logical bits)

1. `Rect` + `Pget` (finish the primitive table).
2. Blit row engine → `Blit`/`BlitKey` (the keystone for the rest).
3. Sprites (`DefineSprite`/`DrawSprite`/`BlitFlip`/`BlitScale`).
4. Double buffering (`SetDrawBuffer`/`Flip`) + background save/restore.
5. Input (`WM_KEYDOWN`/mouse → a key-state table; `KeyDown(vk)`, `MouseX/Y`).
6. GPU profile (index texture + LUT shader + sprite pass).

Then it's a playable engine — [gameaudio.md](gameaudio.md) supplies sound + tunes.
