# GamesCanvas — Library API Guide

A guide for **users of the library**: game authors who want to write a 2D retro
game in hand-written x86-64 (WRASM dialect) without re-writing the window, the
loop, the framebuffer, sprites, tiles, input, or audio every time.

The library ships **two interchangeable backends** that expose the *same public
symbols*:

| backend | folder | surface | present |
|---|---|---|---|
| **CPU** | `library/` | 320×200 indexed framebuffer | GDI `StretchDIBits`, 3× nearest → 960×600 |
| **GPU** | `gpu/` | 640×360 indexed, D3D11 shader compositor | swapchain 1280×720 (×2) or 2560×1440 (×4) |

A finished game is `main` + **three procs** (`init` / `step` / `sprites`). You
switch backends by retargeting one `.include` block — the game body does not
change. Everything below lowers to plain, visible x86-64; inspect any of it with
`was yourgame.was --emit-asm`.

> **Conventions used throughout.** Integer args go in `ecx, edx, r8d, r9d, r10b`
> (Win64-ish), pointers in the full `rcx/rdx/r9`. Return values come back in
> `eax`. Every primitive is a contract-checked `proc` that **clips** to the
> surface (negative/out-of-range coords are rejected by unsigned compare).
> **Index 0 is transparent** everywhere it matters (sprites, tiles). Read the
> canvas size from `CanvasW`/`CanvasH` — **never hardcode 320 or 640** — so the
> same game rides either backend.
>
> **Named constants.** The library ships a `gc_const.was` (one per backend, with
> matching public values) that `canvas.was` auto-includes, so you write
> `ACT_FIRE`, `CANVAS_W`, `PAL_SIZE` instead of magic numbers. They are `equ`
> equates: each folds to the literal you'd have typed (visible in `--emit-asm`),
> so they cost nothing at runtime. Prefer them.

## Label visibility — what your game can call

Module-scoped labels are **on by default** (`was … --nomodules` turns them off,
reproducing the old all-global build byte-for-byte). The rule is simple:

- **Your game file has no `module` marker, so all its labels stay global** — you
  just call the library's exported symbols, exactly as before.
- **Each library file is wrapped in `module Canvas` / `Sound` / `Player` / `Gpu`.**
  Inside a module: a name starting with a **Capital letter is EXPORTED** (global,
  callable from anywhere); a **lowercase/`_` name is PRIVATE** to that module
  (mangled `Module$label`, invisible to you); a **`.globl`'d name is EXPORTED**
  regardless of case (the escape hatch for a lowercase data symbol).

| where the label lives | example | visible to your game? |
|---|---|---|
| your game (no module) | `update_score:` | yes — global |
| library, Capital-initial | `DrawSprite`, `GameRun`, `ParseTune` | **yes — exported** |
| library, `.globl`'d | `fb`, `palette`, `sndBuf`, `gMusicLoop`, `pRTV` | **yes — exported** |
| library, lowercase at module level | `ip_keys`, `actDown` | no — private to its module |
| library, label inside a `proc` | `bx_f_row`, `ad_decay`, `done` | no — private to that proc |

So in practice: **call the capitalised public procs** (every API in this guide is
one) and reference the handful of `.globl`'d data symbols the library exposes
(`fb`/`palette`/`linePal`, `sndBuf`/`sndFrames`, `gMusicLoop`, `pRTV`, …). The
library's lowercase helpers are private — at module level they're module-private
(`Module$label`), and a jump target inside a `proc` is private to that proc
(`proc$label`, so the library reuses names like `loop`/`done` across procs freely).
Either way you can't reach them, and you don't need to. It's byte-neutral
(visibility only renames labels; the machine code is
identical), and you never write a `module` marker yourself unless you want to
organise your *own* multi-file game the same way. Full rules — membership is by
marker not by `.include`, modules span files — are in `help.md` → Modules.

---

## 1. Wiring the library in (the include model)

A library module is a thin **shell `.was` over small fragments**. You `.include`
the modules you need, in dependency order, then write your game. Paths are
relative to *your* file.

**A CPU game:**

```asm
.include "../library/canvas.was"      ; framebuffer + palette LUTs + primitives
.include "../library/blit.was"        ; the blit engine (sprites/scroll build on it)
.include "../library/sprite.was"      ; sprites with per-sprite RGB LUT
.include "../library/tile.was"        ; tilesets + scrolling tile layers
.include "../library/fx.was"          ; LUT effects: fade / flash / colour-cycle
.include "../library/input.was"       ; keyboard + joystick + 8 actions
.include "../library/introspect.was"  ; Snapshot — dump a frame to BMP
.include "../library/harness.was"     ; window + loop + present; AFTER the above
.include "../library/audio/sound/sound.was"  ; SFX synth + live mixer  (optional)
.include "../library/audio/music/abc.was"    ; ABC music player        (optional)
```

**The same game on the GPU** — retarget the folder, nothing else:

```asm
.include "../gpu/canvas.was"          ; idxBuf + palette LUTs + indexed primitives
.include "../gpu/blit.was"            ; the GPU blitter + blend states
.include "../gpu/sprite.was"          ; instanced sprite compositor (unlimited)
.include "../gpu/text.was"            ; instanced 5×7 text layer (Text → DrawText)
.include "../gpu/input.was"           ; identical input API
.include "../gpu/introspect.was"      ; Snapshot (= GpuSnapshot)
.include "../gpu/harness.was"         ; window + loop + GPU compositor present
; GPU-only extras you can opt into:
.include "../gpu/tile.was"            ; tile atlas + parallax bg layers
.include "../gpu/fx.was"              ; procedural fx shader slot
.include "../gpu/mode7.was"           ; per-layer affine (mode-7 floor/ceiling)
.include "../gpu/particles.was"       ; GPU particle system
.include "../gpu/wallfx.was"          ; brick-wall fx
```

> **Ordering matters.** `harness.was` depends on `canvas`/`input`/`introspect`,
> so include it **last**. On the GPU side, `harness.was` also depends on `blit`
> and `text` — include those before it.
>
> **`.balign 16`.** Every library `.DATA` block opens with `.balign 16` to defend
> against the packing trap where an odd-length data declaration upstream
> misaligns a struct/buffer below it. **End your own game's `.DATA` with
> `.balign 16` too** (see `help.md` trap #8) — this is the single most common
> way a hand-written game silently fails to open its window.

---

## 2. The harness — your entry point

The harness owns everything identical across games (the window, the `WndProc`
that fills input, the fixed-step ~60 fps timer, and the resolve→composite→blit
present) and calls back into **your three procs**:

| callback | when | what you do |
|---|---|---|
| `init()`    | once at startup | load sprites/palettes/sounds, build the scene |
| `step()`    | each frame | read input, update state, draw the world into the framebuffer |
| `sprites()` | each frame | composite your sprites over the resolved frame |

The harness API (identical on both backends):

| proc | args | does |
|---|---|---|
| `GameStart` | `rcx`=titleW, `rdx`=&init, `r8`=&step, `r9`=&sprites | create the window, bring up the backend, call `init()` |
| `GameLoop` | — | run interactively (timer + message pump) |
| `GameRun` | same 4 as `GameStart` | **the usual entry** = `GameStart` + `GameLoop` |
| `GameSelfTest` | `rcx`=frames, `edx`=every | **headless**: step `frames` frames, `Snapshot` every `every` |

`titleW` is a wide (`WCHAR`) string pointer. The `QUIT` action (Esc / stick)
closes the window automatically.

> On the **GPU** backend `GameStart` additionally calls `GpuInit(hwnd)` +
> `BlitInit` + `TxtInit` for you, so your `init()` stays device-free — exactly
> like the CPU one. That is what makes the backend switch zero-change.

**A whole game, in skeleton:**

```asm
.globl main
main:
    invoke  GetModuleHandleW, 0           ; (the harness uses this internally too)
    lea     rcx, [rip + gTitle]
    lea     rdx, [rip + Init]
    lea     r8,  [rip + Step]
    lea     r9,  [rip + Sprites]
    call    GameRun
    invoke  ExitProcess, 0

proc Init   frame
    ; SeedLinePalette, DefineSprite, AudioInit, build the board…
endproc

proc Step   uses rbx frame
    ; Action(FIRE), KeyHit(…), move things, Cls + FillRect + Line into the world
endproc

proc Sprites frame
    ; DrawSprite … (composited over the resolved background)
endproc

.DATA
gTitle WCHAR "My Game", 0
.balign 16
```

---

## 3. Canvas — drawing primitives

`canvas.was`. Coordinates are in the **logical** canvas (320×200; the GPU mirrors
that logical size and upscales). All clip to the surface. `index` is a palette
index `0..255`.

| proc | args | draws |
|---|---|---|
| `Cls` | `cl`=index | clear the whole framebuffer to `index` |
| `Pset` | `ecx`=x, `edx`=y, `r8b`=index | one pixel |
| `Pget` | `ecx`=x, `edx`=y → `eax` | read the index at (x,y) |
| `HLine` | `ecx`=x, `edx`=y, `r8d`=len, `r9b`=index | horizontal run |
| `VLine` | `ecx`=x, `edx`=y, `r8d`=len, `r9b`=index | vertical run |
| `FillRect` | `ecx`=x, `edx`=y, `r8d`=w, `r9d`=h, `r10b`=index | filled rectangle |
| `Rect` | `ecx`=x, `edx`=y, `r8d`=w, `r9d`=h, `r10b`=index | rectangle outline |
| `Line` | `ecx`=x0, `edx`=y0, `r8d`=x1, `r9d`=y1, `r10b`=index | Bresenham line |
| `Circle` | `ecx`=cx, `edx`=cy, `r8d`=r, `r9b`=index | circle outline (midpoint) |
| `Disc` | `ecx`=cx, `edx`=cy, `r8d`=r, `r9b`=index | filled circle |
| `Text` | `ecx`=x, `edx`=y, `r8b`=index, `r9`=asciiz | 5×7 bitmap text |

The GPU canvas additionally exposes `FillCircle` and `CanvasSetOrigin(ecx=ox,
edx=oy)` (bias the draw origin within the wider 640×360 buffer; full-screen games
call `CanvasSetOrigin(0,0)`).

---

## 4. The palette / LUT model — colour, the retro way

An **indexed** framebuffer resolved through **independent look-up tables**, each
holding a real 24-bit `0x00RRGGBB` colour. **Index 0 is transparent.** Three LUT
tiers compose:

- **The global 240** — background indices `16..255` resolve through the single
  shared `palette`.
- **Per-line LUT** — indices `0..15` resolve through *that scanline's* own
  16-colour table (`linePal[row][0..15]`). One index → a different colour on
  every row: gradient skies, raster bars, split palettes.
- **Per-sprite LUT** — each sprite carries its own 16-colour bank, independent of
  the line and global LUTs.

Because the LUTs are independent and each holds a real colour, the simultaneous
distinct-colour count *multiplies*: `240 + 16·lines + 16·sprites`.

| proc | args | does |
|---|---|---|
| `SeedLinePalette` | — | initialise every scanline's 0..15 LUT from the global palette |
| `SetLineColour` | `ecx`=row, `edx`=slot 0..15, `r8d`=`0xRRGGBB` | set one colour on one scanline |
| `CyclePalette` | `ecx`=first, `edx`=count | rotate a run of palette entries (animation) |

The colour effects in §8 (`Fade`/`Flash`/`ColourCycle`) are one-line wrappers
over these.

---

## 5. Sprites

A sprite is a small 4-bit (16-colour) bitmap **plus its own RGB LUT**,
composited over the background with index 0 transparent.

### CPU (`library/sprite.was`)

| proc | args | does |
|---|---|---|
| `DefineSprite` | `rcx`=id, `rdx`=asciiz | parse text rows → frame 0; LUT defaults to global 0..15 |
| `SpriteColour` | `rcx`=id, `edx`=index, `r8d`=`0xRRGGBB` | set one of the 16 sprite colours |
| `AddFrame` | `rcx`=id, `rdx`=asciiz | append an animation frame |
| `SetFrame` | `rcx`=id, `edx`=frame | select the current frame |
| `DrawSprite` | `rcx`=id, `edx`=x, `r8d`=y | composite the current frame at (x,y) |

**Authoring a sprite as text** — rows separated by `/`; `1`–`9`/`A`–`F` =
colour index 1..15; `0`, `.`, space = transparent. All rows the same width:

```asm
lea  rdx, [rip + heroArt]
mov  rcx, 0
call DefineSprite
...
heroArt BYTE "..22..../.233320./.233320./..22....", 0
```

### GPU (`gpu/sprite.was`) — unlimited, instanced

The GPU sprite layer batches **all** instances into one `DrawInstanced` call — no
per-scanline cap, thousands of sprites per frame. The model is atlas + per-sprite
palette bank + an instance list:

| symbol | args | does |
|---|---|---|
| `SprInit` | — | create the atlas / palette / instance buffers (the harness or your `init`) |
| `SprAddFrame` | `rcx`=src, `edx`=w, `r8d`=h → frame-id | stamp one frame into the atlas |
| `SprUploadAtlas` | — | push the atlas to the GPU (once, after all `SprAddFrame`) |
| `SprSetPalette` / `SprUploadPalette` | slot, colours | fill and upload a per-sprite 16-colour bank |
| `SprClear` | — | reset the instance list (start of frame) |
| `SprPush` | frame-id, x, y, palette-slot | queue one sprite instance |
| `DrawSprites` | `rcx`=dstRTV | draw every queued instance in one call |

---

## 6. Tiles, tilemaps & scrolling

Tiles are **sprites in a grid**: a tileset of small index bitmaps + a map of one
byte per cell. Tile 0 is transparent, so layers composite; scrolling moves a
start position, no pixels copied. Draw several layers at different scroll rates →
parallax.

### CPU (`library/tile.was`)

| proc | role |
|---|---|
| `DefineTileset` (`rcx`,`rdx`,`r8`) | create a tileset (id, tile w, tile h) |
| `AddTile` (`rcx`,`rdx`) | add a tile shape (text-authored, like a sprite) |
| `TilesetColour` (`rcx`,`rdx`,`r8`) | set a tileset LUT colour (reuse a shape as red/grey/gold) |
| `DrawTile` (`rcx`,`rdx`,`r8`,`r9`) | stamp one tile |
| `DrawTiles` (`rcx`=layer) | paint a layer's visible tile window (+1 margin) |
| `LayerScroll` (`rcx`,`rdx`,`r8`) | set a layer's scroll position |
| `SetTileTarget` / `ClsTarget` | redirect drawing to an offscreen layer buffer |
| `ScrollPresent` (`rcx`,`rdx`) | overscan present: read the world at the smooth scroll offset |

### GPU (`gpu/tile.was`)

`TileInit`, `AddTile`, `DrawTiles`, `TileUpload`, `TileSetScroll`,
`TileRenderLayer` — bg layers are indexed world buffers; per-layer
`scrollX/scrollY` give parallax + overscan as shader constants (zero redraw,
sub-pixel). `TileWorldW`/`TileWorldH`/`AtlasW`/`AtlasH` report the dimensions.

---

## 7. Blit — the engine under sprites/scroll/double-buffer

One primitive underlies sprites, scrolling, background save/restore, and the
double-buffer flip. A **buffer** is any index bitmap.

### CPU (`library/blit.was`)

| proc | args | does |
|---|---|---|
| `DestFramebuffer` | — | target the canvas framebuffer (the default) |
| `SetDest` | `rcx`=base, `edx`=stride, `r8d`=w, `r9d`=h | target a custom surface |
| `SetKey` | `ecx`=index | set the transparent key (default 255) |
| `Blit` | `rcx`=src, `edx`=srcStride, `r8d`=w, `r9d`=h, `r10d`=dstX, `r11d`=dstY | opaque copy |
| `BlitKey` | same as `Blit` | copy, skipping the key index |
| `BlitEx` | same + flags | row-engine variant (mirror / nearest stretch) |

Intra-buffer (`src==dst`) copies pick direction by overlap so a scroll doesn't
corrupt itself; everything clips to the destination surface.

### GPU (`gpu/blit.was`)

The blitter is a textured quad (`src SRV → dst RT`) with modes copy / colour-key
(index 0) / alpha. `BlitInit`, `PoolAlloc` (allocate an asset-SRV or layer-RT by
handle), `BlitToLayer`, `BlitToBack`, `BlitSetKey`.

---

## 8. LUT effects & GPU fx

### CPU (`library/fx.was`) — palette tricks, one-liners

| proc | args | does |
|---|---|---|
| `SaveBase` | — | snapshot the current palette (the baseline to fade from) |
| `Fade` | `ecx`=amount | fade the whole palette toward black |
| `Flash` | `ecx`=amount | flash the palette toward white |
| `ColourCycle` | `ecx`=first, `edx`=count | animate a colour run |
| `Mix` | `ecx`,`edx` | blend the palette toward a target |

### GPU-only (`gpu/fx.was`, `gpu/mode7.was`, `gpu/particles.was`, `gpu/wallfx.was`)

Capabilities the CPU canvas never had:

- **Procedural fx layer** — `FxInit`, `FxSetTime`, `FxRender`: a full-screen
  pixel-shader background (plasma / tunnel / blackhole).
- **Mode-7 affine** — `Mode7Init`, `Mode7SetParams`, `Mode7SetCamZ`,
  `Mode7SetWrap`, `Mode7RenderLayer`: rotate/scale a tile layer into a perspective
  floor.
- **Particles** — `PtInit`, `PtSetAtlas`, `PtBurst` / `PtBurstSprite`, `PtUpdate`,
  `PtDraw`, `PtClear`; tunables `ptSize` / `ptGravity` / `ptFade`.
- **Wall fx** — `WallFxInit`, `WallFxSetTime`, `WallFxRender`, `WallComposite`.

---

## 9. Input

`input.was` (identical API on both backends). Two layers: raw keys + a set of
device-independent **actions** mapped from keyboard *and* joystick.

| proc | args | returns / does |
|---|---|---|
| `KeyDown` | `rcx`=vk | `eax` = 1 if the key is held |
| `KeyHit` | `rcx`=vk | `eax` = 1 if pressed **this frame** (edge) |
| `MouseX` / `MouseY` | — | `eax` = cursor position |
| `Action` | `rcx`=a | `eax` = 1 if the action is held |
| `ActionHit` | `rcx`=a | `eax` = 1 if the action fired this frame (edge) |
| `InputPoll` | — | latch edges, read the stick, recompute actions — **the harness calls this**; you don't |
| `InputKey` / `InputMouse` | — | raw injectors (the harness's `WndProc` calls these) |
| `SimAction` | `rcx`=a, `edx`=down | force an action — for headless self-test play |

**The 8 actions** are exposed as named constants — `ACT_LEFT=0 ACT_RIGHT=1
ACT_UP=2 ACT_DOWN=3 ACT_FIRE=4 ACT_PAUSE=5 ACT_RESTART=6 ACT_QUIT=7` (plus
`ACT_COUNT=8`) — so you pass the name, not a bare integer. They come from
`gc_const.was`, which `canvas.was` auto-includes (see §1), so they are in scope in
any game without an extra include. Default mapping: arrows + WASD, Space/Ctrl =
fire, P = pause, R = restart, Esc = quit; joystick axes = L/R/U/D, buttons 0/1/6/7
= fire/pause/restart/quit.

```asm
mov  ecx, ACT_FIRE     ; folds to 4 — visible in --emit-asm
call ActionHit
test eax, eax
jnz  fire_pressed
```

---

## 10. Introspection & headless testing

Window capture is unreliable here, so the library *dumps a frame to disk* — the
reliable way to see (and machine-check) what was drawn.

| proc | args | does |
|---|---|---|
| `Snapshot` | — | resolve the present buffer and write `snap_<NNNN>.bmp` (sequence-numbered → an ordered filmstrip) |
| `GameSelfTest` | `rcx`=frames, `edx`=every | step a run with nobody at the keyboard, `Snapshot` every N frames |

On the GPU backend `Snapshot` maps to **`GpuSnapshot`** (`CopyResource` back
buffer → staging → BMP). Combine with `SimAction` to *play* the game headlessly:
inject an action, step `GameFrame`, sample. Convert the BMP to PNG (e.g. with
`System.Drawing`) to view it.

> **Two gates for the GPU path.** A BMP readback proves the *render* is correct
> but never exercises the window/device/swapchain. So also run the live window
> once and confirm it appears (`MainWindowHandle != 0`). The BMP can't see the
> bug that hides in the device-creation path.

---

## 11. Audio — SFX (`library/audio/sound/sound.was`)

An SFX is an **offline render** into a float PCM buffer, then either written to a
`.wav` (to hear it with no device) or handed to the live mixer thread. 44.1 kHz /
16-bit / stereo, fully deterministic.

**Render presets** (each fills an `Effect` and renders into the work buffer):

| proc | args |
|---|---|
| `Beep` | `rcx`=freqHz, `edx`=durationMs |
| `Coin` / `Jump` / `Zap` | `edx`=durationMs (factory-tuned) |
| `Explode` | `rdx`=size |
| `Tone` | `rcx`=freq, `edx`=durMs, `r8d`=wave |
| `WriteWav` | `rcx`=pathPtrW — write the last render to a `.wav` |

**Live playback** (the waveOut mixer thread — 8 voices, spinlock-guarded):

| proc | args | does |
|---|---|---|
| `AudioInit` | — | open the device, prepare 4 blocks, start the mixer thread |
| `Play` | `rcx`=sndPtr, `edx`=frames | claim a free voice, play from the start |
| `StopAll` | — | silence every voice |
| `AudioShutdown` | — | signal + join the thread, then reset + close the device |

Typical flow: `AudioInit` once in `init()`; render a preset into `sndBuf`; call
`Play(sndBuf, sndFrames)` on a game event; `AudioShutdown` on exit.

---

## 12. Audio — music (`library/audio/music/abc.was`)

Parse [ABC notation](https://abcnotation.com/) text into a flat, time-sorted
array of MIDI events, then a thread fires them at `midiOut` (Windows' GS synth
makes the sound).

| proc | args | does |
|---|---|---|
| `ParseTune` | `rcx`=asciiz ABC text | parse → the `Tune` event array |
| `WriteTune` | `rcx`=pathPtrW | dump the events as 20-byte records (offline verify) |
| `MusicInit` | — | `timeBeginPeriod(1)` + `midiOutOpen` + start the scheduler thread |
| `MusicThread` | (callback) | the ms-accurate scheduler loop (the OS calls it) |
| `MusicShutdown` | — | stop the thread, all-notes-off, close `midiOut` |

The parser handles headers (`M:`/`L:`/`Q:`/`K:`), accidentals, octaves,
durations, key signatures, bar-local accidentals, rests, chords `[CEG]`, ties,
broken rhythm `a>b`, tuplets `(3CDE`, repeats `|: :|`, multi-voice `V:`, and
`%%MIDI` directives. Timing is drift-free (the cursor is carried as a float;
only emitted stamps are rounded).

---

## 13. Switching backends — the payoff

`gpu/` mirrors `library/` with **identical public symbols** for the canvas,
primitives, palette ops, input, introspection and the harness. To move a game to
the GPU:

1. Repoint the `.include` block from `library/…` to `gpu/…` (§1).
2. Rebuild. The game body is byte-for-byte unchanged.

You then *optionally* light up the GPU-only layers (`gpu/particles.was`,
`gpu/mode7.was`, `gpu/fx.was`, unlimited instanced sprites) that the CPU backend
can't provide — additive, never required for parity.

**Build & run:**

```sh
cargo run -p was -- mygame.was -o mygame.exe
./mygame.exe
```

---

## 14. Quick reference

```
HARNESS   GameRun(title,&init,&step,&sprites)   GameSelfTest(frames,every)
CANVAS    Cls Pset Pget HLine VLine FillRect Rect Line Circle Disc Text
PALETTE   SeedLinePalette SetLineColour CyclePalette        (index 0 transparent)
SPRITES   DefineSprite SpriteColour AddFrame SetFrame DrawSprite
          (GPU) SprAddFrame SprSetPalette SprPush DrawSprites   — unlimited
TILES     DefineTileset AddTile TilesetColour DrawTile DrawTiles LayerScroll
          ScrollPresent     (GPU) TileInit AddTile DrawTiles TileSetScroll
BLIT      DestFramebuffer SetDest SetKey Blit BlitKey BlitEx
          (GPU) BlitInit PoolAlloc BlitToLayer BlitToBack
FX        SaveBase Fade Flash ColourCycle Mix
          (GPU) FxInit/FxRender  Mode7*  Pt*(particles)  WallFx*
INPUT     KeyDown KeyHit MouseX MouseY Action ActionHit SimAction
          actions: ACT_LEFT..ACT_QUIT (0..7), ACT_COUNT=8  — from gc_const.was
INTROSPECT Snapshot   (GPU: GpuSnapshot)
AUDIO     AudioInit Play StopAll AudioShutdown | Beep Coin Jump Zap Explode Tone WriteWav
MUSIC     ParseTune WriteTune MusicInit MusicShutdown
```

See [gamecanvas.md](gamecanvas.md), [gpucanvas.md](gpucanvas.md) and
[gameaudio.md](gameaudio.md) for the design rationale behind each subsystem, and
the `examples/` corpus (`brickout_fx.was`, `gpu_brickout.was`,
`gpu_particles_demo.was`, `parallax.was`) for working games that use this API.
```
