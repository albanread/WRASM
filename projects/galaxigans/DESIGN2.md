# Galaxigans — DESIGN2: the BIG expansion blueprint

> **Premise.** DESIGN.md built the thin shell (8×8 red bugs, peel-and-home dive, 4
> bullets, particle bursts, score/lives, bombs, synth SFX + ABC loop). This document is
> the *major* upgrade: LARGER multi-colour shaded aliens, real wing-flap animation,
> aliens that ROTATE as they swoop (pre-rendered angle frames — the engine has **no**
> per-instance rotation), 4–6 alien types, richer tactics (fly-in entry, LUT swoop arcs,
> boss/escort + capture), the beloved **wah-wah** saucer & power-up sounds, a power-up
> system, more music. Every claim below is pinned to a real engine capability.
>
> **Hard engine facts that shape everything** (from the capability map):
> - `SprPush` carries only **position, palette slot, alpha** — **NO scale, NO rotation**.
>   ⇒ size & rotation are **baked into frames** (flipbook only).
> - **256 atlas frames max**, **512×512** indexed atlas, **32 palette slots × 16 colours**
>   (index 0 = transparent ⇒ 15 usable shades per slot). One `DrawInstanced`, ≤8192 insts.
> - Audio: 4 oscillators (sine/square/saw/tri/pulse/noise), ADSR, **one global LINEAR**
>   pitch sweep (no LFO/vibrato), noise-mix, distortion, echo. Single shared `sndBuf` ⇒
>   `CopySfx` each SFX out immediately. **Vibrato must be faked** (detuned-osc beating, or
>   stepped-pitch `Tone` retrigger — both detailed in §3).
> - **`sprScratch` is currently 1024 B (32×32).** 24×24 = 576 fits; do **not** exceed 32×32
>   per frame, or grow `sprScratch` first.

---

## 0. Frame & palette budget (plan BEFORE authoring — the 256-frame wall is real)

Rotation is the budget killer: a tumbling enemy at K angles × F flap frames = K·F frames.
Use a **shared 8-direction rotation wheel** and keep flap frames low.

| Asset | Size | Anim frames | Angle frames | Total |
|---|---|---|---|---|
| Player ship (3 lean poses: L/0/R) | 20×20 | 3 | — | 3 |
| Bee (Zako) | 16×16 | 2 flap | 8 swoop | 2 + 8 = 10 |
| Butterfly (Goei) | 20×20 | 2 flap | 8 swoop | 2 + 8 = 10 |
| Wasp (erratic) | 18×18 | 3 flap | 8 swoop | 3 + 8 = 11 |
| Boss Galaga (2-HP) | 24×24 | 2 flap + 1 hurt | 8 swoop | 3 + 8 = 11 |
| Saucer (bonus) | 32×16 | 2 shimmer | — | 2 |
| Explosion | 24×24 | 8 | — | 8 |
| Bullet / Bomb / Beam-seg / Star | tiny | 2 / 2 / 1 / 1 | — | 6 |
| Power-up capsules (4 types) | 12×12 | 1 each | — | 4 |
| **TOTAL** | | | | **≈ 75 frames** |

≈75 of 256 frames, comfortably inside the budget with room for a 2nd tier later. The
8-direction **swoop wheel** is the only rotation set per alien (idle/flap uses the upright
frames; the wheel is selected ONLY while diving — see §1.4).

**Palette slots** (32 available): 0 = player, 1 = bee, 2 = butterfly, 3 = wasp, 4 = boss,
5 = saucer, 6 = bullets/bombs/beam, 7 = explosion, 8 = power-ups, 9 = boss-hurt (a
palette-cycled clone of slot 4). Palettes are live textures ⇒ re-call `SprUploadPalette`
for flash-on-hit / tractor-beam shimmer without touching the atlas.

---

## 1. ALIEN ROSTER

Authoring method is unchanged: indexed hex bitmaps decoded by `DecodeSprite`. **Today
`DecodeSprite` only maps `'0'..'9'` → 0..9** (10 indices). For 16-colour art it must read
**`'0'..'9','A'..'F'` → 0..15**. This is a tiny `galaxigans_assets.was` patch (§5 Phase 0):

```
; after 'sub al,'0'': if al<=9 keep; else map 'A'..'F'
cmp al, 9
jbe store
mov al, byte_in            ; reload original char
sub al, 'A'-10             ; 'A'->10 ... 'F'->15
cmp al, 15
jbe store
xor al, al                 ; '.', spaces, junk -> transparent
```

Index 0 stays transparent. All bitmaps below use `.` for transparent and `0-9A-F` for the
15 shade indices.

### 1.1 Palette philosophy — shades-of-a-hue ramps

Each alien gets a dedicated 16-entry slot built as **2–3 short ramps** (dark→bright per
hue) + a dark outline + a specular highlight + an eye-glow. This is what turns the flat
old 4-colour bugs into shaded art. Concrete palettes per type below.

### 1.2 BEE — "Zako" (grunt) — 16×16, slot 1

Lowest rank, most numerous, dives in simple arcs. Blue body + yellow wings (single-hue
ramps per part).

Palette (slot 1):

| Idx | RGB | Role |
|---|---|---|
| 1 | `0x00101830` | outline / shadow (near-black blue) |
| 2 | `0x002A4C8C` | body blue, dark |
| 3 | `0x003E6CC8` | body blue, mid |
| 4 | `0x005A90F0` | body blue, light |
| 5 | `0x008FBEFF` | body blue, specular |
| 6 | `0x00806000` | wing amber, dark |
| 7 | `0x00C09010` | wing amber, mid |
| 8 | `0x00FFD030` | wing amber, bright |
| 9 | `0x00FFF6A0` | wing amber, hot tip |
| A | `0x00FF3040` | eye red |
| B | `0x00FFFFFF` | eye glint / antenna |

Frame-0 (idle, wings UP) — 16×16, fully spelled out:

```
................
......1881......
.....18AA81.....
....1BAAAAB1....
6...12344321...6
76..23455432..67
876.34555543.678
9876.4555554.6789
.9876.55555.6789.
..876.45554.678..
...76.13431.67...
....1..232..1....
....1..222..1....
.....11111111....   (legs)
....1..1..1..1...
....A..A..A..A...   (leg tips, red)
```

Reading: rows 1–3 = the rounded head with two `A` eyes and a `B` antenna glint; rows 4–11
= the fat blue abdomen (`2..5` shade ramp dark→specular down the centre) flanked by amber
wings (`6..9` ramp) that **sweep down-and-out**; bottom rows = legs. Symmetric, mid-detail,
reads instantly as a bee at 16px.

**Animation:** 2 flap frames. Frame-1 (wings DOWN) re-draws only the amber wing block lower
(the `6..9` triangle pivots from `y≈4` up to `y≈9` down) — identical body. Animate at ~0.06
frames/tick (the original's Bee speed).

### 1.3 BUTTERFLY — "Goei" (escort/guard) — 20×20, slot 2

Larger, more aggressive, doubles as Boss escort. Magenta X-wings + white body + orange
head.

Palette (slot 2): `1`=outline `0x00200818`; magenta ramp `2 0x006A1854 / 3 0x00A6308A /
4 0x00D24CC0 / 5 0x00F27CE6 / 6 0x00FFB6F4`; white body ramp `7 0x00808890 / 8 0x00C0C8D0 /
9 0x00FFFFFF`; orange head `A 0x00C04010 / B 0x00FF7020 / C 0x00FFB860`; cyan eyespot
`D 0x0020E0FF`; eye glow `E 0x00FFFFFF`.

20×20, two magenta wing-pairs spreading to the four corners in an X, a slim white central
abdomen (`7..9`), an orange head dome (`A..C`) on top with two `D` cyan eyespots. **Animate
2 flap frames** (wing-tip Y up/down, like the Bee). ~0.05 frames/tick.

### 1.4 WASP — "erratic" (the Galaxian purple weaver) — 18×18, slot 3

Mid-rank; weaves erratically when diving (the Galaxian purple personality). Green/teal
ramp body, stinger tail, fast 3-frame flap.

Palette (slot 3): `1`=outline `0x00081810`; teal ramp `2 0x00134838 / 3 0x001E7A5C /
4 0x002FB484 / 5 0x0058E6B0`; lime wing ramp `6 0x00567A10 / 7 0x008FBE20 / 8 0x00CDF050`;
yellow sting `9 0x00FFE040 / A 0x00FFF6A0`; magenta eyespot `B 0x00FF40C0`; eye `C
0x00FFFFFF`. Three flap frames (up/flat/down) at ~0.08 frames/tick — the fastest flapper.

### 1.5 BOSS GALAGA — "Gorg" (flagship, 2 HP, captures) — 24×24, slot 4

Largest, two hits (first hit = colour-shift to slot 9 "hurt" palette, second hit = die),
runs the tractor-beam capture. Green hull + purple horn-wings + pink core (the iconic
silhouette), with shade ramps.

Palette (slot 4): `1`=outline `0x00041206`; green hull ramp `2 0x00115A18 / 3 0x001E8C28 /
4 0x0030C03C / 5 0x0060E86C`; purple horn ramp `6 0x00401060 / 7 0x00702898 / 8 0x00A048D0
/ 9 0x00C880F0`; pink core `A 0x00FF50FF / B 0x00FFA0FF`; emitter grey `C 0x00586070`;
eye glow `D 0x00FFFFFF`; specular `E 0x00C0FFC0`.

Slot 9 ("hurt") = identical layout but the **green ramp re-tinted purple-red** so a
1-HP boss reads as damaged; selected by swapping the palette slot in `SprPush` (no atlas
change). **Animate:** 2 hull-shimmer frames + 1 "hurt-flash" frame (palette-cycle on slot
9 every other tick for ~12 ticks after the first hit).

### 1.6 The 8-direction SWOOP WHEEL (rotation-as-it-swoops)

The engine cannot rotate, so each alien type carries an **8-frame angle wheel** (N=8,
45° steps) covering the headings a diver actually produces. Diving headings are
"mostly downward, banking ±45° left/right", so 8 frames at 0/45/90/135/180/225/270/315°
fully cover it (the upright idle frame doubles as the 0°/up entry).

**How to bake:** author each wheel frame by hand as a rotated indexed bitmap (8 small
bitmaps per type) OR — cheaper — author ONE upright "swoop pose" (wings swept back, nose
defined) and rotate it offline into 8 bitmaps that you paste into the data file. Either
way they end up as 8 `SprAddFrame` calls into `fBeeRot[0..7]` etc.

**How to pick the frame from velocity (NO trig at runtime):** the dive path is a LUT (§2),
so each path step already stores an integer `(dx,dy)`. Convert to an octant with pure
integer compares — an 8-way `atan2` with no library:

```
; in: dx, dy (signed step deltas from the path LUT). out: octant 0..7
;   octant 0 = up(-y), 2 = right(+x), 4 = down(+y), 6 = left(-x), odds = diagonals
ax = |dx| ; ay = |dy|
if ay >= ax:  vertical-ish:  up if dy<0 else down ; +1 toward the x sign if |dx|*2>ay (diagonal)
else:         horizontal-ish: right if dx>0 else left ; +1 toward the y sign if |dy|*2>ax
```

A compact, fully-integer version: precompute a tiny **16-entry octant LUT** indexed by
`(sign(dx)+1)*... ` — but the compare ladder above is ~10 instructions and needs no table.
Store the chosen octant in `EN_ROT`; `DrawScene` adds it to the type's `fXxxRot` base
frame id. While a diver is in `ST_DIVE` it draws the wheel frame; in `ST_FORM`/`ST_RETURN`
it draws the idle/flap frames (upright).

**Player lean** reuses the same idea with just 3 frames (L/centre/R) keyed off input dx —
no wheel needed.

---

## 2. TACTICS — entry, swoop arcs (LUT path tables), returns, boss/capture, victory

All motion is **closed-form / table-driven, NO runtime trig**. The original used
`SIN/COS/ATAN2(t)`; we bake those into integer LUTs at build time (a Python/awk snippet
emits the `.DATA`, or hand-author). Three path families: **ENTRY**, **DIVE**, **RETURN**,
plus the **boss capture script** and the **victory dance**.

### 2.1 The path-table data format (the core abstraction)

A path is a fixed-step list of **signed byte (dx,dy) deltas** in 640×360 screen space,
applied one per tick on top of an *anchor* (the diver's live formation-slot x,y, so the
swoop tracks the swaying grid — exactly as the original re-anchored each frame).

```
; one path = a header + an array of {dx:i8, dy:i8} steps.
PATH_STEP_STRIDE equ 2          ; {i8 dx, i8 dy}
; header (in .DATA, one per path id):
;   pathLen   : i32   number of steps
;   pathFlags : i32   bit0 = playerVeer tail (butterfly), bit1 = loops (full circle)
;   pathData  : &steps
; an enemy following a path stores: EN_PATHID, EN_PSTEP (current index), EN_ROT (octant)
```

Per tick for a path-follower: read `(dx,dy)` at `EN_PSTEP`, `EN_X += dx ; EN_Y += dy`,
compute octant from `(dx,dy)` → `EN_ROT`, `EN_PSTEP++`. At `EN_PSTEP == pathLen` the
path ends → transition (dive→return, entry→formation, return→formation).

**Baking the arcs (build-time, no runtime trig):** sample the original parametric curves
at the per-tick `t` step and store the *deltas*:

- **DIVE** (the Galaga S-swoop): `x(t)=A·sin(2.5t)`, `y(t)=450t`, `t += 0.004`. Sampled to
  ~180 steps; `dx[i] = round(x(t+dt)-x(t))`, `dy[i] = round(y(t+dt)-y(t))` (≈ +2/tick).
  `A≈90px` in our 640-wide space. Deltas land in ±3 range ⇒ fit i8 easily.
- **RETURN**: `x = SIN(t·π)·40` lateral bulge (0 at both ends), `y` linear top→slot;
  rotation lerps nose-down→upright. Bake as a separate ~150-step path.
- **ENTRY** (the fly-in the original lacks): two mirrored corner-swoops with one loop. Bake
  a left-entry path; the right-entry path is the **same table with dx negated** at load
  (mirror-in-X for free). A "loop" is just a stretch of steps whose dx/dy circle back —
  no special case.

A **single shared sine LUT** (256-entry, `i16`, amplitude ±256) is the only math primitive
needed if you prefer to *generate* deltas at runtime instead of pre-baking them; either way
**no library trig**. Recommended: pre-bake the 3–4 canonical paths to `.DATA` (rigid but
trivial) and add the sine LUT only if paths feel too uniform.

### 2.2 Formation & entry choreography

- **Formation:** keep the grid but enlarge to Galaga's shape — top row 4 Bosses, next two
  rows Butterflies, bottom two rows Bees + a Wasp accent row. Pitch ~50px, sway as today
  (±16, step-down on edge). Slots store `(col,row)→home`; live = home + sway.
- **Entry (NEW state `ST_ENTER`):** at stage start the grid is **empty**; enemies stream in
  along the ENTRY path in single-file trains (a master path + N followers each delayed
  `Δt=6` ticks). `breakIndex` per member = the step at which it peels off to its slot via a
  short lerp. Pattern alternates per stage: P1 dual-mirrored (both corners at once), P2
  single-side double-row, P3 top-loop — selected by `stageEntryPattern = stage % 3`.
- This entry is "free dopamine" (shootable for points) and teaches the wave.

### 2.3 Dive selection & difficulty (the AI brain)

Keep the rotating-scan diver picker but upgrade the cadence model from DESIGN.md:

- **NORMAL:** every ~45 ticks launch one diver from the **bottom-most surviving row**
  (front-line scan). Bee = direct DIVE path; Butterfly = DIVE + **player-veer tail**
  (bit0: the last ~30 steps bias dx toward the player's side, ±1/tick, clamped — the one
  dynamic knob); Wasp = DIVE path with an extra ±x jitter LUT (erratic).
- **FRENZY:** when no pristine originals remain OR `alive < 5`, launch every ~8 ticks, all
  eligible, bomb cadence 25→10. Survivors stop returning and re-dive (persistent-dive).
- **Difficulty table** indexed by `min(stage,8)`: `{diveCadence, bombCadence, diveSpeedMul,
  veerMax, maxConcurrentDivers}`.

### 2.4 Looping returns

Diver path ends (or `EN_Y > FIELD_H+8`) → reserve a landing column (collision-avoidance:
scan other returners' `EN_TCOL`, pick first free) → `ST_RETURN`, teleport to `EN_Y=-30`,
follow the RETURN path into the reserved column → `ST_FORM`. Returners roll upright
(rotation lerps 180°→0° via the RETURN path's octant sequence ending at the up frame).

### 2.5 Boss + escort dive, and the CAPTURE special (the headline mechanic)

Boss has two dive modes (`EN_BOSSMODE`):

- **ESCORT dive:** boss runs the DIVE path; 1–2 Butterflies drawn from the top butterfly
  row fly the *same* path at a fixed `(±14, +4)` offset (formation-flying = sample same LUT
  with a positional offset). Scoring tiers reward killing escorts first (see §5 scoring).
- **CAPTURE dive (TRACTOR):** boss dives **alone** (never while escorted), runs a short
  TOP-LOOP path, then **slides straight down to mid-screen and HOVERS** (`EN_STATE =
  ST_HOVER`, vy=0). It emits a downward **beam cone** = a stack of `fBeamSeg` sprites
  widening downward, palette-cycled (slot 6) for the shimmer. If the player rect is inside
  the cone for the capture window → **FIGHTER CAPTURED**: player turns red, is dragged up,
  docks under the boss; lose a life; boss returns to formation carrying the captive.
  - **Rescue → DUAL FIGHTER:** if the boss carrying your ship dives again and you shoot it
    *mid-dive*, the captive docks beside your active ship → dual fighter (double guns, §4).
  - **Failure rules (faithful):** shooting your own captive destroys it; killing the
    carrier *in formation* makes the captive turn hostile.
  - Plays the **wah-wah capture beam** SFX (§3) the whole hover.

### 2.6 Victory / celebration choreography (the dance)

Two dances, both LUT-driven polar orbits (one shared sine LUT):

- **GAME OVER (aliens won):** survivors orbit screen-centre on a breathing-radius spiral —
  `angle = frame·k + i·phase`, `radius = R0 + sineLUT(frame)·R1`, per-index phase offset =
  pinwheel; each alien draws the swoop-wheel frame nearest its orbit tangent. Plays the
  game-over fanfare.
- **PLAYER WIN (NEW — the original lacked it):** all enemies dead → the player ship loops a
  victory roll (a tiny baked LOOP path cycling its 3 lean frames + the wheel) while a "STAGE
  CLEAR" glockenspiel fanfare plays, then next stage. Reuse the orbit LUT for celebratory
  star bursts (`PtBurst`).

---

## 3. AUDIO — wah-wah, new SFX, richer soundtrack (mapped to OUR real engine)

Our synth: 4 oscs (0 sine/1 square/2 saw/3 tri/4 pulse/5 noise), ADSR, **ONE global LINEAR
sweep** (`fxSweepStart→fxSweepEnd`), `fxNoiseMix`, `fxDistortion`, echo. **No LFO / no
vibrato.** So every "wah" is produced one of two ways:

- **(A) Detuned-oscillator BEATING (no engine change, recommended):** two sines a few Hz
  apart beat at the difference frequency → an amplitude pulse = the "wah". The detune
  *is* the wah rate. Build the Effect block directly (mirror `Coin`): `fxOsc[0]={wave0,
  f0, amp.5}`, `fxOsc[1]={wave0, f0+Δ, amp.5}`, `fxOscCount=2`, set ADSR, `call Render`,
  `CopySfx`. Δ=6 → 6 Hz wah.
- **(B) Stepped-pitch RETRIGGER (no engine change):** for a portamento glide, render N
  short `Tone` segments at stepped pitches into back-to-back regions of one buffer (or
  `Play` them in sequence each tick), giving the discrete-step UFO glide the Galaxian
  divisor-stepping is famous for.
- **(C) Optional ~10-line engine patch** (`sound_render.was` `rn_osc` loop): add
  `fxVibHz`/`fxVibDepth`, modulate `freq*(1+depth·sin(2π·vibHz·t))` before the phase
  multiply. Cleanest, but A+B already deliver every sound below with **zero** engine change.

All SFX follow the proven path: render → **`CopySfx` into a private `.balign 16` real8
buffer immediately** → `Play(buf, frames)` on the event. Add these buffers to
`galaxigans_audio.was` alongside `zapBuf/boomBuf/deathBuf`.

### 3.1 Saucer "wah-wah" (cue 12 — the beloved one)

Two-phase, built with method **A** (beating) + method **B** (the glide):

- **Phase A (pulsed "wah…wah"):** detuned dual-sine, f0=392 Hz (G4), Δ=6 Hz, gate on
  ~190 ms / off ~190 ms ×2. Soft attack (0.04) / soft release (0.08) per gate.
- **Phase B (the warble sweep):** retrigger 7 short Tones stepping the pitch
  G→A→B→C→B→A→G (392→440→494→523→494→440→392 Hz), ~95 ms each, each as a detuned dual-sine
  pair so the beat-wobble rides the glide. `fxNoiseMix=0.03` for breath, `fxDistortion=0.05`
  to fatten (warm-pad feel). Loop while the saucer is on-screen.

`fxOsc` field offsets (from `sound_data.inc`): `+0 wave / +8 freq / +16 amp / +24 phase /
+32 pulseWidth`, stride 40; `fxOscCount` at +168; ADSR at +176/184/192/200. Set these
directly then `call Render` (no preset helper exists — presets write the block directly).

### 3.2 Power-up "wah-wah" / EXTEND sparkle (cue 13)

Bright ascending bell arpeggio + an optional wah. Method **B**: retrigger `Tone(wave=0
sine, +octave partial via a 2nd osc)` up a C-major run C5→E5→G5→C6 (~70 ms each, sharp
attack ~0.005 / fast decay ~0.25, no sustain = bell envelope). For the "wah" lift, make
each Tone a detuned dual-sine (Δ=5) so the bell shimmers. Reads as "power-up / level up".

### 3.3 New gameplay SFX (all data-only via the Effect block / presets)

| SFX | Recipe (our API) |
|---|---|
| **Player fire** | `Zap(80)` already, or `Tone(900, 70, wave=2 saw)` + fast falling sweep `fxSweepStart=1200,fxSweepEnd=400`. |
| **Enemy dive/swoop** | falling sweep 1000→300 over ~0.3 s on saw (wave 2) + `fxNoiseMix=0.1`; for the whine, dual-detune (Δ=8) so it warbles down. |
| **Capture beam** | sustained: dual pulse (wave 4, `pulseWidth=0.12`) detuned Δ=20 ⇒ deep beating, `fxSweepStart=400,fxSweepEnd=900` (slow rise), `fxNoiseMix=0.05`. The "twirly hypnotic" beam — loop while hovering. |
| **Enemy kill** | `Explode(320)` (today's boom). |
| **Boss death** | `Explode(600)` + `fxDistortion=0.2` + echo (`fxEchoCount=2,fxEchoDelay=0.05,fxEchoDecay=0.5`). |
| **Player death** | `Explode(800)` (today's deathBuf). |
| **Extra-life EXTEND** | §3.2 sparkle. |
| **Power-up pickup** | `Coin` preset, or §3.2 with a final octave-up grace note. |

### 3.4 Richer multi-track soundtrack + fanfares (ABC → `ParseTune`/`MusicInit`)

Keep the inline-ABC approach in `galaxigans_data.inc`/`_audio.was`. Add cues (program
numbers per the GM palette the original used — 80 square lead, 9 glockenspiel, 52 choir,
91 pad):

- **Gameplay bed (NEW — the original had none):** a looping 2-voice march, square lead
  (prog 80) + square/tri bass alternating root-fifth, Q≈150, minor key (Am/Cm) — sits
  between the slow title (84) and fast win (234). This replaces the current thin Am loop as
  the in-`ST_PLAYING` track.
- **Title / stage-start:** slow Am choir-pad build (prog 52, Q=84), 4 bars, looped.
- **Saucer appear sting:** the cue-12 pad run (prog 91) — but the *audible* wah is the SFX
  in §3.1; the ABC cue is the musical stab.
- **Victory fanfare (STAGE CLEAR):** ascending C-major glockenspiel arpeggio (prog 9,
  Q=234) — the existing cue-13 melody.
- **Game over:** Cm square-lead march (prog 80, Q=210) — the existing cue-10 melody, the
  richest one; loops under the alien victory dance.

Trigger discipline: `IF NOT MUSICPLAYING(n) THEN MUSIC PLAY n` for loops; fire-once on
state entry for fanfares. Store each tune as its own `BYTE ...,0` ABC string; `ParseTune`
the active one on state change.

---

## 4. POWER-UPS (tie the saucer/capture to a reward economy)

A small capsule system. **Drops:** killing the **bonus saucer** drops a capsule; rarely a
killed Boss drops one. Capsule = a `fPowerCap` sprite (slot 8) that falls slowly
(`dy=2`); catching it with the ship applies the effect.

`POW_*` pool (4–6 slots, stride 12 `{x,y,kind}`). Kinds:

| Kind | Effect | Duration | Visual |
|---|---|---|---|
| `POW_RAPID` | fire cooldown 8→3 | ~600 ticks | yellow capsule, bullet trail |
| `POW_SPREAD` | fire 3 bullets (−1/0/+1 dx) | ~600 ticks | cyan capsule |
| `POW_SHIELD` | 1 free hit (absorbs next bomb/diver) | until consumed | blue ring sprite around ship |
| `POW_DUAL` | dual fighter (2 ships side-by-side, 4 bullets) | until death | green capsule |
| `POW_EXTRA` | +1 life | instant | white capsule, EXTEND jingle |

**Dual-fighter** is also granted by the **capture→rescue** path (§2.5) — the marquee way to
get it; the capsule is the easy way. Dual fighter widens the hitbox (faithful trade-off)
and doubles `BULLET_MAX` effective fire. Each pickup plays the §3.2 power-up wah; EXTRA
plays the EXTEND sparkle.

State: `powRapidT`, `powSpreadT`, `powShield`, `dualFighter` flags/timers in
`galaxigans_data.inc`; applied in `PlayerStep`/`BulletsStep`; HUD shows active power-ups.

---

## 5. IMPLEMENTATION PLAN — ordered, each phase BUILDABLE + BMP-VERIFIABLE

Every phase ends with a working `galaxigans.exe` and an FXTEST filmstrip showing the new
feature. Respect the fragment structure (`module Galaxigans` per file, Capitalised
cross-fragment procs, `DecodeSprite`/`SprAddFrame`/`SprSetPalette`/`CopySfx` conventions,
`GpuSnapshot` before `Present` in the FXTEST gate). New `.DATA` lives in
`galaxigans_data.inc`; `.balign 16` guards each real8 buffer.

**Scoring upgrade (folds into Phase 5/6):** Bee 50/100 (form/dive), Butterfly 80/160,
Boss 150/400, Boss+1 escort 800, Boss+2 escorts 1600, saucer 100, transform-set bonus.
Diving worth 2× resting — the core risk/reward dial. Update `score`/`FormatScore`.

### Phase 0 — 16-colour decoder + atlas/scratch headroom *(touches: `_assets.was`,`_data.inc`)*
Patch `DecodeSprite` to map `'A'..'F'`→10..15 (§1). Confirm `sprScratch` ≥ 24×24 (576 ≤
1024 ✓; if any 32×32 art is added later, it's still ≤1024). **Verify:** rebuild, existing
art still renders (BMP unchanged) — pure capability unlock.

### Phase 1 — Bigger shaded aliens (art only) *(touches: `_data.inc`,`_assets.was`,`_draw.was`)*
Author the Bee (16×16, §1.2 spelled-out), Butterfly (20×20), Wasp (18×18), Boss (24×24)
indexed bitmaps + their slot-1..4 palettes (`SetupSprPalettes`). Build their idle frames,
upload. Map formation rows to the new types in `DrawScene`. **Verify:** filmstrip shows the
new multi-colour fleet at proper sizes (no rotation/anim yet).

### Phase 2 — Flap animation *(touches: `_data.inc`,`_assets.was`,`_draw.was`)*
Add the 2nd/3rd flap frames per type; a per-enemy `EN_ANIM` phase counter advanced in
`FormationStep`; `DrawScene` picks `fXxx[frame]`. **Verify:** filmstrip across ~30 frames
shows wings flapping.

### Phase 3 — Swoop rotation wheel *(touches: `_data.inc`,`_assets.was`,`_enemies.was`,`_draw.was`)*
Bake the 8-frame wheel per type (`fBeeRot[8]` …). Add `EN_ROT`; compute octant from the
dive `(dx,dy)` (the integer ladder, §1.6); `DrawScene` draws the wheel frame while
`ST_DIVE`, idle/flap otherwise. **Verify:** filmstrip of a dive shows the alien banking
through angles.

### Phase 4 — LUT path tables (entry + dive + return) *(touches: `_data.inc`,`_enemies.was`)*
Add the path format (§2.1) + bake DIVE/RETURN/ENTRY paths to `.DATA`. Convert
`FormationStep` divers from the hand-coded peel-home to path-following. Add `ST_ENTER`
fly-in at `ResetGame`/stage start; `ST_RETURN` docking. **Verify:** filmstrip shows the
fly-in assembling the formation, then a clean S-swoop and a return arc.

### Phase 5 — Boss/escort + scoring *(touches: `_enemies.was`,`_collision.was`,`_data.inc`)*
Boss 2-HP (first hit → slot-9 hurt palette), escort group-dive (followers at fixed offset),
escort-first scoring tiers. **Verify:** filmstrip: a boss dives with two butterflies in
formation; first hit recolours, second kills.

### Phase 6 — Capture beam + dual fighter *(touches: `_enemies.was`,`_player.was`,`_collision.was`,`_data.inc`,new `_powerup.was`)*
`ST_HOVER` boss + beam-cone sprites (palette-cycled), capture → lose ship → carried;
rescue mid-dive → dual fighter. **Verify:** filmstrip: beam emits, ship captured (red,
dragged up); a scripted rescue forms the dual fighter.

### Phase 7 — Power-up system *(new `galaxigans_powerup.was`; touches `_data.inc`,`_collision.was`,`_player.was`,`_draw.was`,`_main.was`)*
`POW_*` pool, saucer/boss drops, capsule fall + catch, the 5 effects + HUD. `GameStep`
calls `PowerupStep`. **Verify:** filmstrip: saucer dies → capsule falls → ship catches →
spread-shot visible.

### Phase 8 — Audio expansion *(touches: `_audio.was`,`_data.inc`)*
Add the wah-wah saucer & power-up buffers (§3.1/3.2 — detuned-osc beating + stepped Tone),
dive/capture/extend SFX, and the new ABC tracks (gameplay bed, title, fanfares, game-over).
Wire triggers in the relevant steps. **Verify:** run interactively (audio is not in the BMP
gate) — the saucer wah, power-up sparkle, dive whine, and the gameplay bed are audible;
filmstrip confirms no regressions.

### Phase 9 — Victory dance + multi-stage *(touches: `_enemies.was`,`_main.was`,`_data.inc`)*
Player-win loop + alien game-over orbit (shared sine LUT); stage counter scales the
difficulty table + entry pattern. **Verify:** filmstrip of the win loop and the game-over
spiral.

**New files:** `galaxigans_powerup.was` (Phase 7), optionally `galaxigans_paths.inc`
(baked LUTs, Phase 4) included by the shell. **Changed files:** `_data.inc` (every phase
adds state/equates), `_assets.was` (Phases 0–3 art), `_enemies.was` (3–6,9),
`_collision.was` (5–7), `_player.was` (6–7), `_draw.was` (1–3,7), `_audio.was` (8),
`_main.was` (7, callback wiring). The shell `galaxigans.was` adds the new `.include`s.

---

## 6. PHASE B — the animated shader background (layer 0) *(slots after Phase 1)*

The user wants a **moving shader background on layer 0** — a scrolling starfield / drifting
nebula under the whole scene. Mapped from the engine (the `gpu/fx.was` capability study):

**Engine facts.** `gpu/fx.was` is a ready-made full-screen procedural shader template
(`FxInit` / `FxSetTime` / `FxRender(rcx=dstRTV)`): an HLSL string → fullscreen triangle →
a 16-byte `float4 tparm` cbuffer (`tparm.x` = animating time) → drawn FIRST, opaque, as the
bottom layer. Custom shader = clone fx.was, replace the `FPS` body, keep the
`SV_VertexID` fullscreen-triangle VS verbatim. Compile via `D3DCompile` (already imported).

**The catch — present graduation.** The shared `gpu/harness.was` `GameFrame` is hard-wired
to `GpuComposite`, which **clears the back buffer opaque + resolves the indexed board
opaque** — nothing can show behind it. So a layer-0 background requires **graduating to an
author-owned present** (exactly what the original `DESIGN.md` §present prescribed, and what
`brickout_fx` does). Keep `GameStart` for window+init+GameInit, but route the timer to a
custom `GxFrame` instead of the harness `GameFrame`:

```
GxFrame:
  TxtClear ; InputPoll ; GameStep            ; logic + DrawBackground(Cls 0) + queue HUD text
  StarSetTime ; StarRender(pRTV)             ; LAYER 0 — starfield/nebula, FIRST, opaque
  WallComposite                              ; resolve idxBuf with INDEX-0 DISCARD, alpha-over (no clear)
  TxtUpload ; RSSetViewports(vp) ; TxtFlush(pRTV)
  GameSprites                                ; SprUpload + DrawSprites(pRTV) + PtDraw(pRTV)
  pSwap.Present(1,0)
```

Z-order (bottom→top): **starfield shader → indexed board (idx0 discarded) → HUD text →
fleet sprites → additive particles.** `DrawBackground` stays `Cls(0)` (index 0 now = "show
the sky through", because `WallComposite`'s PS discards index 0).

**New module** `galaxigans_starfield.was` (clone of `gpu/fx.was`, symbols
`StarInit`/`StarSetTime`/`StarRender`; HLSL = 3 parallax star layers scrolling down + a
nebula tint, `tparm.x` driving the scroll). Shell also `.include`s `gpu/wallfx.was` (for its
`WallComposite` index-0-discard resolve — its brick shader is unused). `WallFxInit` +
`StarInit` called in `GameInit`. **Verify:** filmstrip shows the fleet over a moving
starfield; index-0 areas reveal the sky.

**Order:** do Phase 0 → Phase 1 (bigger aliens) → **Phase B** (so the new fleet flies over a
living sky early — the headline visual), then resume Phase 2+ (anim, rotation, paths, …).
Phase B is isolated (rendering only), low coupling to the gameplay phases.
