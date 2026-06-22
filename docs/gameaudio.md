# GameAudio (RASM) — design & status

Our design doc for sound, hand-written in x86-64 via WRASM. Derived from the M2
reference implementation (`E:\NewModula2\library\winrtmod\` — `Abc.mod`, `Audio.mod`,
the waveOut/midiOut sinks) and the spec `E:\NewModula2\GameAudio.md`, written against
our toolchain. Companion: [gamecanvas.md](gamecanvas.md).

Two halves, and this is the **harder** stack than graphics — three things the canvas
never had to do: *a lot* of string parsing (ABC), timing that must not drift, and a
**second thread** (the device drains audio continuously; the game can't stall it). The
saving grace, and the architecture we copy from M2: **all the expensive work happens
once, up front** (synth render, ABC parse). The playback thread then does nothing but
*dispatch* — no parsing, no allocation, no float DSP in the hot path. That's what makes
the timing tractable.

## Library layout

Audio roughly doubles the library, so it gets its own tree:

```
library/audio/
  sound/      SFX: offline synth → PCM, waveOut mixer thread        (deterministic, self-contained)
  music/      tunes: ABC text → MIDI events, midiOut scheduler thread (the parser is the work)
```

(The existing graphics/input/harness modules stay flat in `library/` for now; if that
grows we can group them `library/graphics/` + `library/sys/` later — a separate, churny
move since it rewrites every demo's `.include` paths.)

## Status

| Area | Module | State |
|---|---|---|
| Waveforms + ADSR + effect render → PCM | sound | ⬜ |
| SFX presets (beep/coin/jump/zap/explode…) | sound | ⬜ |
| WAV writer (RIFF/WAVE, 16-bit) — the audio `introspect` | sound | ⬜ |
| waveOut mixer thread (32 voices, double-buffered) | sound | ⬜ |
| ABC → MIDI-event parser | music | ⬜ |
| midiOut scheduler thread | music | ⬜ |

Nothing built yet — design ahead of the next phase. Every piece maps to existing tools
(SSE float math, `invoke` for winmm, `proc`/`frame`, `CreateThread`).

## Format & determinism

- **44100 Hz, 16-bit, stereo** PCM throughout (`nBlockAlign = 4`, `nAvgBytesPerSec = 176400`).
- **Deterministic**: a sound renders byte-reproducibly (known-answer unit-testable).
  Noise from one LCG, `x = x*1103515245 + 12345 (mod 2^32)`, seed `12345`:
  - signed `[-1,1)`: `((x>>16) & 0x7FFF)/16384 - 1`
  - unit `[0,1)`: `((x>>16) & 0x7FFF)/32768`
- Pitch: `A4 = MIDI 69 = 440 Hz`, `freq = 440 * 2^((n-69)/12)`.

---

# sound/ — the SFX synth

A sound is an **offline render** into a float PCM buffer, then written to `.wav` or
handed to the mixer thread. Everything is SSE scalar/packed float; `libm` calls
(`sin`/`tanh`/`pow`) are visible.

## The `Effect` (the recipe) and `Sound` (the result)

```
Sound:   sampleRate u32, channels u32, count u32, duration f64, samples f64*   ; interleaved L,R in [-1,1]
                                                                               ; max 10s stereo = 882000 floats
Adsr:    attack f64, decay f64, sustain f64, release f64                       ; seconds; sustain 0..1
Osc:     wave u32, frequency f64, amplitude f64, phase f64, pulseWidth f64
Effect:  duration f64
         osc[4], oscCount u32                       ; stacked oscillators
         env Adsr
         sweepStart f64, sweepEnd f64               ; pitch glide in Hz (equal = off)
         noiseMix f64                               ; 0..1 dry/noise blend
         distortion f64                             ; 0 = off
         echoCount u32, echoDelay f64, echoDecay f64
```

Default ADSR for a fresh effect: `0.01 / 0.1 / 0.7 / 0.2`.

## Waveforms (evaluated at a phase in radians)

| wave | formula |
|---|---|
| sine | `sin(phase)` |
| square | `rem(phase,2π) < π ? 1 : -1` |
| saw | `n=phase/2π; 2*(n - floor(n+0.5))` |
| triangle | `t=rem(phase/2π,1); 4*abs(t-0.5) - 1` |
| pulse | `t=rem(phase,2π)/2π; t < pw ? 1 : -1` |
| noise | LCG signed `[-1,1)` |

`rem(x,m) = x - m*floor(x/m)` (Euclidean, non-negative).

## Render pipeline — exact per-frame order

```
clamp duration to [0.01, 10.0]; frameCount = 44100*dur (stereo); dt = 1/44100
for f in 0..frameCount:
    t = f*dt;  s = 0
    1. for i in 0..oscCount:  s += wave(osc[i], 2π*osc[i].freq*t + osc[i].phase) * osc[i].amp
    2. sweep:  if sweepStart != sweepEnd:  freq = lerp(sweepStart,sweepEnd, t/dur);  s += sin(2π*freq*t)*0.5
    3. noise:  if noiseMix>0:  s = s*(1-noiseMix) + lcgSigned()*noiseMix
    4. env:    s *= adsrAt(env, t, dur)
    5. drive:  if distortion>0:  d=1+distortion*10;  s = tanh(s*d)/tanh(d)
    6. write:  samples[f*2+0] = samples[f*2+1] = s*0.5            ; 0.5 = headroom
7. echo (post): delayFrames = round(echoDelay*44100)
                for e in 1..echoCount:  add samples[i] * echoDecay^e  at offset delayFrames*e
8. normalize:   peak = max|sample|;  if peak>1: scale all by 0.9/peak   ; quiet sounds untouched
```

ADSR (`adsrAt(env, time, noteDur)`): attack ramp 0→1, decay 1→sustain, hold at sustain
for `noteDur - (a+d+r)`, release sustain→0; each segment guards a zero-length stage.

## Presets (factory → fills `Effect` → `Render`)

`Beep(freq)`, `Coin`, `Jump` (sweep 300→600), `Zap` (sweep 1000→100 + noise),
`Shoot`, `Explode(size)` (two low oscs + sweep 135→32 + distortion), `Powerup`,
`Hurt`, `Click`, `Bang`, `Blip(pitch)`, `Tone(freq,wave)`. Also colored `Noise(kind)`
— white / pink (Paul Kellet 3-pole) / brown (integrated, clamped) with a quadratic
fade-out.

## PCM + WAV

- `PutI16(x)`: `i = round(clamp(x,-1,1)*32767)`; store little-endian.
- `WriteWav`: 44-byte RIFF/WAVE header + the i16 data. **This is the audio
  `introspect`** — render offline, write a `.wav`, *hear* it with no device, the way
  `Snapshot` lets us *see* a frame.

---

# music/ — the ABC player

The real work. Parse [ABC notation](https://abcnotation.com/) into a flat, **time-sorted
array of MIDI events**, then a thread fires them at `midiOut`. No synthesis — Windows'
GS synth makes the sound; we send the right bytes at the right ms.

## What it produces

```
MidiEvent: timeMs u32, status u32, chan u32, d1 u32, d2 u32   ; 0x90 on, 0x80 off, 0xC0 prog, 0xB0 ctrl
Tune:      ev[16384], count u32, bpm u32, endMs u32
```

Parse is **single-pass, char-by-char** (no separate tokenizer) — a state machine over
the text, emitting a NoteOn at the running `curMs` and a matching NoteOff at
`curMs + dur`, then advancing `curMs`.

## Grammar handled (this is the "a lot of string parsing" part)

- **Header fields** (before the first `K:`): `M:` meter, `L:` default note length,
  `Q:` tempo (bpm), `K:` key — `K:` snapshots the defaults and starts the body.
- **Notes**: `^ ^^ _ __ =` accidentals, `A–G` (octave 4) / `a–g` (octave 5),
  octave marks `'` (+12) `,` (−12, stackable).
- **Durations** relative to `L:`: bare `N` → `N·L`; `/` → `L/2`; `/M` → `L/M`;
  `N/M` → cross-multiply `(N·unitN)/(M·unitD)`; trailing dots `.` each ×3/2.
- **Key signature**: sharp order `F C G D A E B`, flat order `B E A D G C F`; applies a
  default accidental per pitch class.
- **Bar accidentals**: an explicit `^/_/=` overrides the key and **persists to the end
  of the bar** (`barAcc[7]`, `barSet[7]` indexed A–G); **a bar line `|` resets them**.
- **Rests** `z`/`Z`, **chords** `[CEG]` (emit together, advance once), **ties** `a-`
  (sum the fractions of same-pitch notes), **broken rhythm** `a>b` (×3/2, ×1/2; `<`
  reversed), **tuplets** `(p[:q[:r]]` (next `r` notes scaled `q/p`; `q`,`r` defaults by
  `p`), **repeats** `|: … :|` (single-pass expansion, first occurrence),
  **inline fields** `[K:…] [Q:…] [L:…] [M:…] [V:…]`, **grace `{…}`/slurs `()`** ignored,
  **`%%MIDI program/channel/transpose`** directives, **`%`** comments.
- **Multi-voice** `V:id`: each voice saves/restores its own `{curMs, key, unit, meter,
  transpose, chan, instrument, barAcc[]}`; channel 9 (percussion) auto-skipped.

Pitch: `midi = baseOctave*12 + semitone(letter) + accidental + 12*octaveMarks +
transpose`, clamped `0..127`. Semitones `C0 D2 E4 F5 G7 A9 B11`.

## Timing model — and why it stays tight

All durations are computed in **whole-note units** as exact integer fractions, turned
to ms only at emit:

```
msPerWhole = timeSigDenom * 60000 / bpm
startMs = round(curMs)
endMs   = round(curMs + durWholes * msPerWhole)      ; round(x) = floor(x+0.5)
curMs  += durWholes * msPerWhole                      ; carry the float, round only the stamps
```

Carrying `curMs` as a float and rounding only the *emitted* stamps means **rounding
error never accumulates** — the cursor stays exact, so a 3-minute tune ends on the
right beat. Ties/durations stay rational (`dn/dd`, cross-multiplied, reduced by gcd)
right up to that one float multiply.

## Finalization

1. Emit a **Program Change (0xC0) at timeMs 0** for every voice (initial instrument).
2. **Stable sort by `(timeMs, priority)`** with priority `NoteOff(1) < Ctrl(2) <
   Prog(3) < NoteOn(4)` — so at a shared timestamp the old note releases *before* the
   new one starts (key composite: `timeMs*16 + priority`).
3. Record `endMs = max timeMs`.

After this the tune is immutable; the scheduler only reads it.

---

# Threading & playback (the part with teeth)

The device pulls audio on its own clock; if our thread is late the user hears a gap.
So playback runs on a **dedicated thread** (`CreateThread`), and the game talks to it
only through small, locked state. Two independent sinks:

## sound/ — waveOut mixer thread (sample-accurate)

- **Setup**: `waveOutOpen(WAVE_MAPPER, &fmt)` with the PCM `WAVEFORMATEX`; allocate **4
  blocks** of **2048 frames** (8192 bytes each), `waveOutPrepareHeader` each.
- **Voices** (32): `{active, looping, fading, snd*, framePos, gainL, gainR, fadeGain,
  fadeDec}`. `Play(snd,vol,pan)→handle` finds a free slot and sets L/R gain from pan;
  `StopVoice(h,fadeSecs)` sets `fadeDec = 1/(fadeSecs*44100)`.
- **Thread loop (~2 ms)**: under the lock, for each of the 4 blocks, if
  `(flags & WHDR_INQUEUE)==0` → software-mix 2048 frames (sum all active voices ×gain
  ×fade ×master, clamp, `PutI16`), then `waveOutWrite` it. `Sleep(2)`.
- **Timing is the device's job**: the card consumes exactly 44100 frames/s, so as long
  as a block is always ready, playback is sample-accurate by construction. The only
  failure mode is *underrun* (thread too slow) → keep the mix branch-light.

## music/ — midiOut scheduler thread (ms-accurate)

- **Setup**: `timeBeginPeriod(1)` (1 ms timer res), `midiOutOpen(MIDI_MAPPER)`.
- **Tracks** (4: one music + three SFX cues), each `{tune*, idx, startMs, on}` plus a
  per-track **held-note table** (`key[24] = chan*128 + note + 1`) so stopping one cue
  silences only its own notes. Channel 15 reserved as a never-silenced drone/pad.
- **Thread loop (~1 ms)**: under the lock, `now = timeGetTime() - startMs`; while the
  next event's `timeMs <= now`, `midiOutShortMsg(status|chan | d1<<8 | d2<<16)` and
  update the held table; at end, all-notes-off for that track. `Sleep(any? 1 : 10)`.
- ms resolution is plenty for music (the GS synth has its own attack); the pre-sorted
  event array means the loop is a pointer compare + a `midiOutShortMsg`, nothing more.

## Locking — keep it transparent

The game thread mutates voices/tracks (Play/Stop); the audio thread reads them. Guard
the shared state with either a **critical section** (`InitializeCriticalSection` /
`Enter`/`Leave` — standard, blocking) or, more in the spirit of the toolchain, a
**spinlock** we can read in the listing: `lock xchg` a flag to acquire, `mov [flag],0`
to release. Critical sections are short (a few field writes), so contention is nil.

## The thread proc is a Win64 callback — mind the ABI

`CreateThread`'s start routine, like `HWndProc`, is called *by the OS*: it takes its arg
in `rcx`, must **preserve every non-volatile** and return in `rax`. Same lesson the
harness firewall taught us — write it as a `proc … uses <all non-volatiles> frame`, or
the OS gets back trashed registers. (And anything the audio thread touches that the game
thread also touches needs the lock — a data race here is a click or a crash, not a
visible glitch.)

---

## Hard parts / gotchas (collected)

- **Accidentals reset per bar**, persist within it — the classic ABC trap; the bar line
  must clear `barAcc/barSet`.
- **No rounding drift**: carry `curMs` as float, round only emitted stamps (above).
- **Same-timestamp ordering**: NoteOff before NoteOn at equal ms, or notes choke.
- **Tuplet/duration math** stays in integer fractions (gcd-reduced) until the last step.
- **waveOut underrun**: the mix loop must always beat the 2 ms refill; no `malloc`, no
  parsing, no `sin` in it (all pre-rendered).
- **Thread shutdown**: signal the loop, join the thread, then `waveOutReset`/`Close`
  (or `midiOutReset`/`Close`) and `timeEndPeriod(1)` — order matters or the device hangs.
- **The thread callback ABI** (above) — a non-volatile slip corrupts the OS, silently.

## Toolchain mapping

| need | tool |
|---|---|
| winmm/kernel (`waveOut*`, `midiOut*`, `timeGetTime`, `CreateThread`, file I/O) | `invoke` (DB signatures) |
| float DSP (osc, ADSR, normalize), `sin`/`tanh`/`pow` | SSE — `real4`/`real8`, auto-float marshaling; `libm` via `invoke` |
| the PCM buffer, event array, wavetables | `.DATA` / `.include` fragments |
| render & parse loops | `proc`/`frame`, `.if`/`.while`, contract checks |
| the lock | `lock xchg` spinlock (visible) or a critical section |
| "hear it offline" | `WriteWav` — the audio `introspect` |

## Roadmap (logical bits)

1. **sound/**: oscillators + ADSR → a single enveloped `Beep` → `WriteWav` → *hear* it (known-answer unit test on the PCM bytes).
2. **sound/**: the full effect pipeline (sweep, noise, distortion, echo, normalize) + the preset SFX.
3. **sound/**: the waveOut mixer thread (start with one voice, then the 32-voice mix + the lock).
4. **music/**: the ABC parser → `Tune` event array, unit-tested against known tunes (note count, timestamps, sort order).
5. **music/**: the midiOut scheduler thread → play a tune under the game loop.

Then: **Canvas + audio + tunes + input = 2D games.**
