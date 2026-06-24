# Galaxigans — design note

> **Status:** scaffold only (created the evening before). Not yet buildable — the
> fragments are header-stubbed TODOs. Tomorrow's work fills them in.

A Galaxian/Galaga-style fixed shooter — our **first multi-file game project** (not an
`examples/` monolith). It's the natural next showcase after BrickOut FX: where brickout
exercised the canvas + one ball + a handful of packs, **galaxigans stress-tests the
sprite fleet** — dozens of enemies, diving AI, two projectile pools, collision under
load — plus the same audio stack (ABC soundtrack + synth SFX).

## The reference

Port/extend the FreeBASIC example the user shared:

- **`E:\NewFB\bas\demo\16_galaxigans.bas`** — "Galaga Sprite Demo: High-Res Scene"
  (NewFB / "FasterBASIC"), 1447 lines. `SCREEN 640,480`.

The user is **thinking of extending** that example (more than a 1:1 port — e.g. extra
enemy types, levels, the bonus saucer fully fleshed out, attract mode). Treat the .bas
as the gameplay spec / source of truth for the mechanics, then go beyond it.

NewFB's primitives mirror the WRASM framework ~1:1 (see [[showcase-game-references]] in
memory), so this is **concept translation (game logic → procs), not new framework**.
Primitive usage in the .bas: `SPRITE` ×247, `PALETTE` ×55, `PSET` ×43, `MUSIC` ×27
(inline ABC), `GCLS` ×17, `SOUND` ×14 (presets), `GKEYDOWN`/`GINKEY` input,
`GCLS/update/FLIP/VSYNC` loop.

## Gameplay model (from the .bas globals)

- **Player**: `px,py`, 3 `ships` (lives), `fire_cooldown`, `shoot_held` /
  `shoot_release_frames` (tap-to-fire, no autofire), `p_death_timer` (death/respawn beat).
- **Formation**: `formation_x/y/dir` — the grid sways side-to-side and descends; enemies
  sit in it when not diving. `returnee_count` tracks divers coming back.
- **Enemies** (up to 60): per-enemy `alive`, `dive` (dive progress / path param),
  `return`, `join_row`, `target_col` — i.e. enemies **break formation to dive at the
  player on an arc, then fly back and rejoin their slot**. That diving AI is the heart of
  Galaga and the most interesting port.
- **Player bullets** (4): `bullet_x/y/dx/active` — capped to 4 in flight (the .bas DIMs
  `bullet_*(3)`).
- **Enemy bombs** (≈10): `bomb_active/x/...` — divers drop bombs at the player.
- **Bonus saucer**: `saucer_active/spawn_timer/x/y/dx/exploding` — periodic fly-across
  for bonus points.

## Library subsystems exercised (the showcase)

| BASIC primitive | WRASM module |
|---|---|
| `SPRITE DEF/SHOW` (247) | `gpu/sprite.was` — atlas frames + per-sprite palettes, instanced `DrawSprites` |
| `PALETTE` (55) | `SprSetPalette` slots (per enemy type / player / saucer) |
| `SOUND ZAP/SHOOT/EXPLODE/BIGEXPLODE` (14) | `library/audio/sound`: **`AudioInit`** (mixer thread — at boot, before any `Play`); presets `Zap(rdx=ms)`/`Coin`/`Explode(rdx=size)`/`Tone` render into one **shared** work buffer → `CopySfx` each into a private `real8` buffer *immediately*, then `Play(rcx=buf, edx=frames)` |
| `MUSIC LOAD` inline ABC (27) | `library/audio/music`: `ParseTune(rcx=ABC ptr)` → `gMusicLoop=1` → `MusicInit`; the CC7 volume duck (`MusicSetVol`/`MusicDuckStep`) is author-owned (copy from brickout) |
| `GKEYDOWN`/`GINKEY` | `gpu/input.was` |
| explosions | `gpu/particles.was` `PtBurst` (reuse the brickout pattern) |
| HUD (score/lives) | `gpu/text.was` |
| backdrop / starfield | `gpu/canvas.was` (+ maybe a procedural star scroll like `wallfx`) |

## Carry-overs from BrickOut FX (proven patterns)

- **Pools marshalled through scratch globals**: brickout ran one ball physics core over a
  pool by copying each record in/out. The enemy/bullet/bomb pools here are simpler
  (independent records), so a plain per-record loop is enough — but the pool idiom (fixed
  stride, `active` flag, find-free-slot) is identical.
- **Sprite assets synthesized at boot** (`BuildSprAssets` → `SprAddFrame` ×N →
  `SprUploadAtlas`, palettes → `SprUploadPalette`). Author player/enemy/saucer/bullet/
  bomb/explosion frames as indexed bitmaps (index 0 transparent).
- **Present** is **author-rolled COM**, not a library call: the frame tail is
  `pContext.OMSetRenderTargets` → background composite → `PtDraw` → `SprUpload` +
  `DrawSprites(pRTV)` → `TxtUpload` + `TxtFlush` → `pSwap.Present(1,0)` (see
  `examples/brickout_fx.was:2429-2463`; drop its `WallFxRender`/`WallComposite` — wallfx
  only — for a plain canvas/starfield composite). Z-order: clear → game step (queues
  sprites + text) → particles → sprites → text. Sprites live in 640×360 space.
- **Author-owned (NOT library — write/copy these; no fragment may call them as a library
  symbol)**: `CopySfx` (copy `proc CopySfx uses rbx rsi rdi r12 in rcx rdx` verbatim from
  `brickout_fx.was:231`); the present tail above; the index-0-discard background
  composite; and `BuildSprAssets`/`SetupSprPalettes`/`DrawScene`/`ResetGame`/`GameStep`
  + the `GxBoot/GxFrame/GxLoop`/`GxSelfTest` harness (renamed off brickout's `Fx*`).
- **Headless `FXTEST` BMP gate** for verification (window capture fails here — dump BMP,
  convert to PNG, read it: [[verify-graphics-demos-via-bmp]]).
- **Traps**: `.balign 16` **before each** `real8` SFX buffer (not once per block)
  ([[rasm-data-alignment-trap]]); COM `.Method` clobbers register args; in the FXTEST
  gate, `GpuSnapshot` must run **before** `Present` (DISCARD swap effect) or the BMP is
  black.
- **Label scoping** (the module system — this *supersedes* the old "prefix everything
  `gx_/en_/bul_`" rule): wrap **each** game code fragment in `module Galaxigans …
  endmodule`. A `module` marker scopes only the lines physically in its own file, and
  `.include` does **not** absorb a child into the includer's module — so the marker goes
  at the top of every fragment, exactly as the library repeats `module Gpu` in each file.
  Then: jump labels inside a `proc` are proc-private (`loop:`/`done:`/`row:` reusable
  across procs), lowercase module-level labels are module-private, and **Capitalised**
  names export across fragments (the procs you call between files). Only shared
  `.DATA`/equate names (`ENEMY_MAX`, `*_STRIDE`, pool bases) live in one global namespace
  — keep *those* unique. No more hand-prefixing.

## Fragment plan (multi-file, shell-of-includes)

`galaxigans.was` is a thin shell: header + the gpu/library `.include`s + the game
fragments. Each fragment opens `module Galaxigans` and emits its own `.DATA`/`.CODE` head
(section switches are idempotent); cross-fragment procs are Capitalised (auto-exported).
Include the **gpu/\*** family only — `Cls`/`Text`/the input API also live in `library/*`,
and pulling in both is a duplicate-label error. See [[shell-of-includes-convention]].

- `galaxigans_data.inc` — all game state (.DATA): pools, player, formation, saucer,
  timers, sprite frame-id cells, palette table, ABC soundtrack text.
- `galaxigans_assets.was` — `BuildSprAssets`: synthesize the indexed sprite frames +
  set palettes + upload.
- `galaxigans_enemies.was` — formation sway/descent + per-enemy dive/return AI + enemy
  bomb drops.
- `galaxigans_player.was` — input → move/fire/death + player-bullet update.
- `galaxigans_collision.was` — bullet↔enemy, bomb↔player, diver↔player; scoring;
  explosion bursts (`PtBurst`).
- `galaxigans_audio.was` — bake SFX + wire the ABC soundtrack; pickup/explosion hooks.
- `galaxigans_draw.was` — queue the scene sprites + HUD text (the `DrawScene` analogue).
- `galaxigans_main.was` — `GxBoot`: window class → `GpuInit`/`BlitInit`/`SprInit`/
  `TxtInit`/`PtInit` → `BuildSprAssets`/`SetupSprPalettes` → **`AudioInit`** + bake SFX +
  `ParseTune`/`MusicInit` → `ResetGame`; then `GxFrame` (input → `GameStep` → present
  tail) under `GxLoop`; `AudioShutdown`/`MusicShutdown` on exit. `IFDEF FXTEST` →
  `GxSelfTest` (BMP gate; `GpuSnapshot` *before* present).

## Open questions

1. **Include set + namespace** — resolve FIRST (most likely first-build failure): pin the
   gpu/* family + `library/audio`, wrap each fragment in `module Galaxigans`. (Settled.)
2. Resolution/scale: reuse the **640×360** GPU logical space (the sprite/canvas stack runs
   there); pick the vertical playfield rect + the logical→screen sprite transform.
3. Dive-path: baseline a precomputed `.DATA` per-step `dx,dy` LUT indexed by a dive
   counter (no library trig — bake the Galaga arc at build/boot); reserve runtime math
   (one shared sine table) only if data paths prove too rigid.
4. Enemy count + formation layout (the .bas allows 60). Start with the .bas's grid.
5. How far to "extend" beyond the .bas — levels? attract mode? a second enemy tier?
