# Capture Boss — design

The marquee mechanic, ported from Galaga's "Boss Galaga". It creates the game's best
risk/reward loop: the boss can **abduct** your ship with a tractor beam; you can **rescue** it
by killing the boss, earning a **dual fighter** (double firepower) — but a careless shot
destroys your captured ship instead. This doc is the spec; implementation is phased (§11).

---

## 1. The player fantasy
1. A big, menacing boss descends and opens a glowing tractor beam.
2. Caught in it, your ship is pulled up into captivity — you lose it, but it isn't destroyed.
3. The boss carries your ship back to the top, prisoner in tow.
4. You shoot the boss to free your ship → it flies down and merges → **two ships, twice the guns**.
5. The tension: while it holds your ship, a shot that hits the *prisoner* destroys it for good.

---

## 2. The boss entity — placement decision
**Decision: a dedicated entity, NOT a formation grid member.** The fleet grid is 16×16 on a
fixed pitch; a 32×32 boss would collide with neighbours and complicate the sway/dive loop. So
the boss is its own object with its own lifecycle (like the saucer), stationed *above* the
formation so it still reads as leading the wave.

- **Appears** on flagged levels (add a `LV_BOSS` column to `levelTable` — ties into the
  data-driven level system; e.g. from L3 onward, or every Nth level).
- **Toughness:** 2 hits. **Bounty:** +1000 (more if it holds a prisoner you rescue).
- One boss at a time.

---

## 3. The boss sprite — 32×32, detailed, animated
Twice the fleet's size (32×32 is exactly our `sprScratch` decode max). Authored procedurally
(like ship / saucer / mine) and render-reviewed before commit. Its own palette slot (10),
hue-shaded.

- **Look:** regal and threatening — deep purple/magenta carapace, gold edge trim, two wing-like
  mandibles, antennae, and a central **cyan emitter eye** (the tractor source) so the beam
  clearly originates from it. Bilaterally symmetric.
- **Animation frames:**
  - **Idle** ×2 — mandibles flex, antennae sway (looped while stationed).
  - **Charge** ×2–3 — the eye brightens dim→blazing, energy arcs gather, body leans forward.
  - **Beam** ×2 — eye at max, throbbing.
  - **Hit-flash** ×1 — white flash on the first (non-fatal) hit.

---

## 4. The tractor beam — the special effect
A vertical cone from the emitter, narrow at the boss, widening downward to ~2× ship width at the
capture line. Additive blend so it glows over whatever cosmos is behind it. Animated **scan-bands
scroll downward**, with an alpha pulse and a slight width wobble. It **grows** over the ~30-frame
charge (telegraph) and **holds** for ~90 frames.

**Rendering — the careful part.** A single baked beam sprite is ruled out: it would be ~32×210,
far past the 1024-byte (32×32) sprite-decode buffer. Three viable options, decided by a Phase-1
spike:
- **(A) Dedicated additive beam shader** — a thin procedural quad following the `fx.was` /
  starfield pattern (its own VS/PS + one draw). Best fidelity (taper, scan-lines, glow all
  procedural, any length). Most infra.
- **(B) Additive particle stream** — emit glowing particles from the emitter in a widening
  downward cone each frame; the stream *is* the beam. Reuses the particle system, very dynamic,
  no decode-size limit. Needs a particle-budget check.
- **(C) Stacked 32×32 segments** — ~7 cone-slice sprites stacked vertically, scan-bands by frame
  cycling. Simplest, no new infra, but the most sprite frames and a 32px width cap.

Recommendation: prototype **(A)** for the look; fall back to **(C)** if shader time is short.
The *capture hit-test is geometric* (point-in-cone), independent of whichever renderer wins.

---

## 5. State machine
`bossState`:
- `BS_IDLE` — absent; `bossTimer` counts to the next appearance.
- `BS_ENTER` — flies in from the top to its station point.
- `BS_STATION` — hovers, idle-animating; `bossTimer` counts to the next capture run. May also
  run ordinary attack dives.
- `BS_DESCEND` — swoops to a capture position above the player's current x, at the capture line.
- `BS_CHARGE` — stops; beam grows; charge animation (~30 f). Telegraph — the player can still flee.
- `BS_BEAM` — beam active (~90 f). Each frame: if the ship is in the cone → **capture** (§6).
- `BS_RETURN` — beam off; flies back to station (carrying the prisoner if it caught one).
- `BS_DYING` — took the 2nd hit; explodes. If holding a prisoner, **frees** it (§7).

Capture/player flags: `captureHeld` (0/1), `prisonerX/Y` (drawn position, tracks the boss),
`playerDual` (0/1).

---

## 6. The capture moment
- Tested only during `BS_BEAM`. Cone test: ship's centre-x within the cone half-width at the
  ship's y, and ship alive.
- On capture: a brief **tractor animation** floats the ship up the beam to dock above the boss;
  then `pAlive=0` (respawn on the next life), `captureHeld=1`, prisoner parented to the boss.
- Fairness: the beam is **visible during CHARGE before it can capture**, so the player always
  gets a telegraph to dodge.
- If captured with **no lives left** → game over, prisoner lost (no rescue possible). Acceptable.

---

## 7. The rescue → dual fighter
While `captureHeld` and the boss is back in station:
- Killing the boss (2nd hit) **frees** the prisoner: it detaches, flies down to the live ship,
  and merges → `playerDual=1`.
- **Dual fighter:** two ships ~12px apart; `SpawnBullet` fires **two** bullets (one per hull);
  wider hitbox; drawn as two ship sprites.
- **Damage buffer:** a hit while dual costs **one** ship (back to single, `playerDual=0`) instead
  of a life.

## 8. The friendly-fire risk (the iconic tension)
In station the boss sits **below** the prisoner, so upward bullets hit the boss first → safe
rescue. The tension comes from the **boss diving with the prisoner in tow**: during that dive the
captured ship can be in front of your guns, and a bullet that hits the **prisoner** destroys it
permanently (no dual fighter). So: shoot the boss in station to rescue safely; hold fire when the
prisoner leads the dive. `CheckPrisonerHit` enforces the loss.

---

## 9. Data & integration
- **State:** `bossState/X/Y/HX/HY/Anim/Timer/Hits/BeamT`, `captureHeld`, `prisonerX/Y`,
  `playerDual`. New palette slots: boss (10), beam (11 if shader/sprite).
- **GameStep (PLAYING):** `BossStep`, `CheckBeamCapture`, `CheckBossHit`, `CheckPrisonerHit`.
- **DrawScene:** `DrawBoss` + `DrawBeam` + `DrawPrisoner` + (dual) the 2nd ship.
- **Player path:** `SpawnBullet` twin-fire; `CheckPlayerHit/Bomb/Mine` honour the dual buffer.
- **ResetGame:** clear boss/capture/dual state.
- **Levels:** `LV_BOSS` flag in `levelTable` decides which levels field a capture boss.
- **Audio (new Wah-style presets):** beam charge = rising whine; capture = descending swoop;
  rescue = triumphant chime; boss death = a bigger Explode.

---

## 10. Edge cases
- Boss killed mid-beam → cancel the capture.
- Capture while dual → lose one ship (to single), not a full abduction.
- Stage cleared while the boss holds a prisoner → carry the dual/again next stage? Keep simple:
  clearing the stage frees nothing; the prisoner is lost unless rescued first.
- Determinism: drive boss timing from the frame counter / `Rng` so the FXTEST stays reproducible.

---

## 11. Phasing
1. **Spectacle** — boss entity + 32×32 animated sprite + beam (ENTER→STATION→DESCEND→CHARGE→
   BEAM→RETURN), no capture. Tune the look on the filmstrip. *(decide the beam renderer here)*
2. **Capture** — ship caught in the cone → prisoner docked; lose a ship.
3. **Rescue + risk** — kill boss → dual fighter; twin-fire + damage buffer; prisoner friendly-fire.
4. **Polish** — audio, boss-dives-with-prisoner tension, escorts, `LV_BOSS` level flagging.

Start with Phase 1: get the boss and beam on screen and beautiful before any abduction logic.
