# GPU Canvas — design + sprint plan

The GPU profile of GamesCanvas: a **Direct3D 11 shader backend** that mirrors the CPU
canvas (`library/`) primitive-for-primitive, so a game switches backends by changing
one `.include` path. The slogan: **a 16-bit console with the brakes off** — the SNES/
Genesis playbook (tile layers, hardware sprites, palette banks, parallax, mode-7) with
the era's limits thrown away (unlimited sprites, unlimited layers, mode-7 on everything,
fx shaders), all GPU-accelerated at runtime.

This doc is the **reference an agent reads before starting any sprint**. Keep it current.

---

## 1. Profiles & resolution

| profile | base buffer | aspect | present | notes |
|---|---|---|---|---|
| **CPU** (`library/`) | 320×200 indexed | 16:10 | GDI `StretchDIBits` 3× → 960×600 | the reference, exact, portable |
| **GPU** (`gpu/`) | **640×360 indexed** | **16:9** | swapchain **1280×720 (×2)** default, **2560×1440 (×4)** "pixel-double" option | the lo-res-but-widescreen step-up |

640×360 integer-scales dead clean (×2 = 720p, ×4 = 1440p) → crisp nearest-neighbour, no
half-pixels. **Games read the canvas dimensions (`CanvasW`/`CanvasH`), never hardcode 320
or 640**, so the same game rides either backend.

---

## 2. Per-frame pipeline

```
indexed layers (640×360, R8_UINT)
   → palette-LUT resolve in pixel shaders (the indexed soul stays alive)
   → composite back-to-front
   → present (autoflip swapchain, 720p/1440p)
```
After the **start-time** asset upload, the per-frame cost is **pure GPU**: the CPU only
bumps scroll / sprite-position / palette constants and issues the draw calls. Zero
per-pixel CPU work.

---

## 3. Layer stack (back → front)

| # | layer | kind | notes |
|---|---|---|---|
| 0 | **fx** | RGBA, procedural pixel shader | plasma / tunnel / blackhole — ported CPU demos |
| 1 | **graphics** | indexed | free-draw primitives (Cls/FillRect/Line/Text-into-layer) |
| 2 | **bg0** | indexed tile layer | own scrollX/scrollY |
| 3 | **bg1** | indexed tile layer | own scroll → parallax + overscan = constants, zero redraw |
| 4 | **bg2** | indexed tile layer | own scroll |
| 5 | **sprites** | indexed, per-sprite palette | alpha-over, colour-0 transparent, **UNLIMITED** (no per-scanline cap) |
| 6 | **text** | font atlas | top |

Fixed roles (retro-PPU style) + a small free asset pool. Each layer is a 640×360
render-target; a layer is drawn into (or is procedural), then composited into the back
buffer with its blend + optional **affine transform** (mode-7). The fx layer is RGBA; the
rest resolve through palettes at composite time.

---

## 4. Texture / buffer model

Three kinds of GPU texture (the "VRAM"):

- **Asset SRVs** — uploaded once at start, read-only: tile atlas (`R8_UINT`, e.g. 512×512),
  sprite atlas (`R8_UINT`, 512×512), font atlas (`R8_UINT`).
- **Palette LUTs** (`B8G8R8A8_UNORM`): global `256×1`; per-line `16×360` (one 16-colour row
  per scanline); per-sprite `16×slots`.
- **Layer RTs** — drawn every frame: fx / graphics / bg0..2 / composite targets (`640×360`,
  `R8G8B8A8_UNORM` for composited, or `R8_UINT` world buffers for the index layers).
- **Swapchain back buffers** — DXGI owns the front/back **autoflip** (`Present()` flips).
  You don't hand-manage 0/1.

The **GPU blitter is the workhorse**: a textured quad, **src SRV → dst RT**, modes
`{copy, colour-key (index 0 transparent), alpha-blend}`. It's `library/blit.was` reborn as
a draw call — every layer op (stamp a tile, composite a sprite, flatten a layer) goes
through it.

---

## 5. Palette resolve — the indexed soul (do NOT resolve to RGBA early)

A **background** pixel, index `c`, scanline `sy`:
```hlsl
col = (c < 16u) ? perLine.Load(int3(c, sy, 0))   // low 16 = this scanline's palette
                : global.Load(int3(c, 0, 0));     // 16..255 = the global 240
```
A **sprite** pixel, index `c`, palette slot `s`:
```hlsl
if (c == 0u) discard;                              // colour 0 transparent
col = spritePal.Load(int3(c, s, 0));               // per-sprite 16-colour bank
```
So every CPU palette trick survives, free: **palette cycling** = re-upload the 256×1 LUT;
**per-line raster bars / gradient skies** = the 16×360 texture; **per-sprite palettes** =
the slot row. This is the whole reason to stay indexed.

---

## 6. D3D11 mapping — reuse `examples/mandel_gpu.was`

The plumbing already exists; lift it:
- **Device + swapchain**: `D3D11CreateDeviceAndSwapChain`, `DXGI_SWAP_CHAIN_DESC`
  (`R8G8B8A8_UNORM`, 1280×720; FLIP_DISCARD/2-buffer is the modern path, DISCARD/1 is what
  the demos use today — start with the demo's, upgrade later).
- **Shaders**: HLSL embedded via `.ASCIISTRING` + `D3DCompile` at runtime; fullscreen-
  triangle VS from `SV_VertexID` + `Draw(3,0)`; `vs_4_0`/`ps_4_0`.
- **Index texture**: `Texture2D<uint>` sampled with `.Load()` (integer fetch, no filtering).
- **Upload**: `CreateTexture2D` + `UpdateSubresource`; `CreateShaderResourceView`;
  `CreateSamplerState` (POINT + CLAMP — crisp pixels).
- **Composite**: `OMSetRenderTargets` to a layer RT (or the back buffer), draw the
  fullscreen triangle with the layer's shader; `Present(1,0)`.
- **COM calls**: the `comobj Iface : TYPE` + `pIface.Method(args)` macro (winkb resolves the
  vtable slot + ABI marshalling). **This is the key plumbing** — see `mandel_gpu.was` lines
  165–304 and `docs/macros.md` 95–125.

---

## 7. Parity & the switch

`gpu/` mirrors `library/` with **identical public symbols** (`Cls`/`FillRect`/`Text`/
`DrawTiles`/`DrawSprite`/palette ops/`CanvasW`/`CanvasH`/the harness). A game switches by
pointing its `.include` at `gpu/` instead of `library/` — **zero game changes**. The seam
is a present-**backend** (CPU = resolve+blit; GPU = the compositor). A runtime toggle (both
linked, pick at init) is a later bonus.

---

## 8. Verification — bake in the lesson

The `.data`-alignment bug this session hid in exactly the path headless BMP verification
*cannot* reach (it never built a window class). So **every sprint has TWO gates**:

1. **BMP readback** (`GpuSnapshot`): `CopyResource` back buffer → staging texture → `Map` →
   write a BMP. The headless analog of `introspect.was`/`Snapshot`. We `Read` the PNG.
   *Proves the render is correct.*
2. **Live window**: actually run it and confirm the swapchain creates + presents + a window
   appears (`MainWindowHandle != 0`). *Proves the device/swapchain/present path works* — the
   thing BMP testing can't see.

Plus: `gpu/` modules **must `.balign 16` every `.DATA` block** (D3D structs + the shader
`.ASCIISTRING` source are alignment-sensitive) — see `help.md` trap #8.

---

## 9. Folder layout

```
gpu/canvas.was   — device/swapchain, the LUT background shader, present, GpuSnapshot,
                   the buffer pool, CanvasW/CanvasH, the drawing primitives (into layers)
gpu/blit.was     — the GPU blitter (src→dst quad: copy / colour-key / alpha)
gpu/tile.was     — tile atlas + bg layers + per-layer scroll (parallax/overscan)
gpu/sprite.was   — sprite atlas + per-sprite palette + the sprite pass (unlimited)
gpu/text.was     — font atlas + text layer
gpu/fx.was       — the procedural fx shader slot
gpu/harness.was  — the GPU present-backend (mirrors library/harness.was, swaps present)
```
Same names as `library/` so a game's `.include` just retargets the folder.

---

## 10. Sprints

Model (same as the audio sprints): an agent drafts the WRASM + self-tests via the build
loop; the main loop rebuilds / reviews / **verifies (BMP + live-window)** as the gate. Each
sprint is independently buildable and verifiable, and builds on the last.

### G0 — Spine (the spike)
**Build:** D3D11 device + swapchain at 1280×720; upload a 640×360 `R8_UINT` index test
pattern + a palette LUT; the palette-LUT pixel shader (global + per-line); fullscreen
present; `GpuSnapshot` (back buffer → BMP).
**Deliverable:** a recognisable indexed 640×360 image, crisp at 720p, as a **live window**
*and* a dumped **BMP**.
**Proves:** device / swapchain / shader / upload / `R8_UINT` LUT / present / readback — the
whole spine in one shot.
**Files:** `gpu/canvas.was`, `examples/gpu_spike.was`.

### G1 — Texture pool + blitter
**Build:** the buffer-pool API (alloc asset-SRV / layer-RT by handle); the GPU blit
(textured quad, modes copy / colour-key / alpha); the layer-RT compositor skeleton.
**Deliverable:** blit an asset texture into a layer RT, composite that layer to the back
buffer.
**Files:** `gpu/blit.was`.

### G2 — Tile layers + scroll (parallax / overscan)
**Build:** tile atlas upload; bg layers as indexed world buffers; `DrawTiles` into a layer;
per-layer `scrollX/scrollY` → parallax + overscan as constants (zero redraw, sub-pixel).
**Deliverable:** 3 parallax tile layers scrolling smoothly.
**Files:** `gpu/tile.was`.

### G3 — Sprites (brakes off)
**Build:** sprite atlas + per-sprite palette LUT; the sprite pass (batched/instanced quads,
alpha-over, colour-0 transparent); **no per-scanline cap**.
**Deliverable:** a swarm of hundreds–thousands of sprites, all drawn every frame — the
moment the 16-bit limits visibly fall away.
**Files:** `gpu/sprite.was`.

### G4 — Text + fx + full compositor
**Build:** font atlas → text layer; the procedural fx shader slot (port `plasma` + one more,
e.g. a blackhole); wire the full stack `fx → graphics → bg0/1/2 → sprites → text → flip`; the
present-backend seam.
**Deliverable:** the complete compositor, every layer live.
**Files:** `gpu/text.was`, `gpu/fx.was`, `gpu/harness.was`.

### G5 — Parity + a real game
**Build:** `gpu/` exposes the same API as `library/`; take an existing game (brickout or
parallax) and run it on the GPU backend by retargeting its `.include` from `library/` to
`gpu/`, unchanged otherwise.
**Deliverable:** a real game, all-GPU, at 720p, identical logic — verified side-by-side with
its CPU build.

### G6 — Bonuses + the payoff demo
**Build:** per-layer affine (mode-7 rotate/scale); the 1440p pixel-double option; GPU palette
cycling; a showcase that flexes the brakes-off — thousands of sprites + an fx background + a
mode-7 floor.
**Deliverable:** the demo that shows what the CPU canvas never could.
