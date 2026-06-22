# library/audio/sound — SFX synth

Deterministic offline synthesis → PCM, played live through a `waveOut` mixer thread.

- An `Effect` recipe (stacked oscillators + ADSR + frequency sweep + noise mix +
  distortion + echo) → `Render` → a float `Sound` buffer → `PutI16` → `waveOut`
  (or `WriteWav` to *hear* it offline — the audio `introspect`).
- Presets: `Beep` / `Coin` / `Jump` / `Zap` / `Shoot` / `Explode` / `Powerup` /
  `Hurt` / `Click` / `Bang` / `Blip` / `Tone`, plus colored `Noise` (white/pink/brown).
- 44100 Hz, 16-bit stereo. Noise from the fixed LCG → byte-reproducible (known-answer
  unit-testable).

Design: [../../../docs/gameaudio.md](../../../docs/gameaudio.md) (the **sound/** section).
Reference: `E:\NewModula2\library\winrtmod\Audio.mod`.

Status: not built yet — design only.
