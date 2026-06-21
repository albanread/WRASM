# GameAudio (RASM) — design & status

Our design doc for sound, hand-written in x86-64 via WRASM. Derived from the
language-agnostic spec (`E:\NewModula2\GameAudio.md`), written against our
toolchain. Companion: [gamecanvas.md](gamecanvas.md). Two halves: a deterministic
**SFX synth** (the fun, self-contained one) and an **ABC-notation music player**
(the real challenge).

## Status

| Area | State |
|---|---|
| SFX synth (waveforms + ADSR + render → PCM) | ⬜ |
| WAV writer (RIFF/WAVE, 16-bit) | ⬜ |
| Playback (`waveOut`) | ⬜ |
| ABC → note-event parser | ⬜ |
| MIDI playback (`midiOut`) | ⬜ |

Nothing built yet — this is the design ahead of the next phase. Every piece maps
cleanly to existing tools (SSE float math, `invoke` for winmm, `proc`/`frame`).

## Format & determinism

- **44100 Hz, 16-bit, stereo** PCM throughout.
- **Deterministic** so a sound is byte-reproducible (and unit-testable by
  known-answer). Noise from a fixed LCG: `x = x*1103515245 + 12345 (mod 2^32)`,
  seed `12345`. Pitch: `A4 = MIDI 69 = 440 Hz`, `freq = 440 * 2^((n-69)/12)`.

## SFX synth (design)

A sound is an offline render into a PCM buffer (`Sound`), then either written to a
`.wav` or pushed to `waveOut`. Pipeline, all SSE scalar/packed float:

```
oscillator → frequency sweep → noise mix → ADSR envelope → echo → normalize → PCM
```

- **6 waveforms**: sine, square, saw, triangle, noise (LCG), and a pulse/blend.
- **ADSR**: attack/decay/sustain/release in samples; a per-sample multiplier.
- **Sweep**: linear/exponential frequency glide over the sound's length.
- **Echo**: a delay line with feedback (a ring buffer of past samples).
- **Normalize**: scan for peak, scale to full-scale to avoid clipping.

API shape (register convention as in the canvas):

```
proc Render   ; (params struct ptr) → fills a Sound buffer with float samples
proc ToPcm    ; float[-1,1] → i16 interleaved stereo
proc WriteWav ; Sound → RIFF/WAVE file (44-byte header + data)
proc Play     ; Sound → waveOutOpen/Write/Close (winmm via invoke)
```

`WriteWav` doubles as introspection for audio: render offline, write a `.wav`, and
*hear* the result without a device — the audio analogue of `introspect.was`.

## Music: ABC → MIDI (design)

The harder half — parse [ABC notation](https://abcnotation.com/) into timed note
events and stream them to the system synth via `midiOut`.

- **Parser** (the real work): tokenize ABC — note letters `A–G`, accidentals
  `^ _ =`, octave marks `, '`, durations `2 /2 /`, rests `z`, bars `|`, and the
  header fields (`K:` key, `L:` unit length, `M:` meter, `Q:` tempo). Output: a
  list of `(pitch, start_tick, duration)` events.
- **Scheduler**: walk events on a tick clock; emit `midiOutShortMsg` Note-On /
  Note-Off (status `0x90`/`0x80`, channel, pitch, velocity) at the right times.
- **Tempo**: `Q:` (quarter-notes/min) + `L:` set ticks→ms.

This needs no synthesis — Windows' built-in GS synth makes the sound; we just send
the right messages at the right moments. A small state machine over a text buffer:
exactly the kind of thing the toolchain's `proc`/`.if`/`.while` are for.

## Toolchain mapping

| Need | Tool |
|---|---|
| winmm/kernel APIs (`waveOut*`, `midiOut*`, file I/O) | `invoke` (DB signatures) |
| float DSP (osc, envelope, normalize) | SSE — `real4`/auto-float marshaling |
| big tables (wavetables, the PCM buffer) | `.DATA`, `.include` library fragments |
| structured render/parse loops | `proc`/`frame`, `.if`/`.while`, the contract checks |
| "hear it offline" | `WriteWav` (the audio `introspect`) |

## Roadmap (logical bits)

1. Oscillators + ADSR → a single `Beep` (sine, enveloped) → `WriteWav` → verify.
2. The full render pipeline (sweep, noise, echo, normalize) + a few preset SFX.
3. `waveOut` live playback.
4. ABC parser → note-event list (unit-tested against known tunes).
5. `midiOut` scheduler → play a tune.

Then: **Canvas + audio + tunes + input = 2D games.**
