# library/audio — sound for the game framework

Two independent sinks, each on its own thread (the device drains audio continuously, so
playback can't live on the game loop):

- **sound/** — SFX: deterministic offline synth → PCM → a `waveOut` mixer thread.
- **music/** — tunes: ABC notation → a time-sorted MIDI-event array → a `midiOut`
  scheduler thread (Windows' GS synth makes the sound).

The architecture that makes it tractable: **all the heavy work (synth render, ABC
parse) happens once, up front**; the playback threads then only *dispatch* — no
parsing, no allocation, no float DSP in the hot path.

Design + the M2-derived implementation notes: [../../docs/gameaudio.md](../../docs/gameaudio.md).
Reference implementation: `E:\NewModula2\library\winrtmod\` (`Abc.mod`, `Audio.mod`).

Status: design only — not built yet.
