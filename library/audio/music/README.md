# library/audio/music — tune player

ABC notation text → a time-sorted MIDI-event array → played live through a `midiOut`
scheduler thread (Windows' GS synth makes the sound — no synthesis here).

- **The parser is the work**: a single-pass, char-by-char state machine over ABC —
  notes, accidentals (with per-bar persistence), octave marks, durations + dots, key
  signatures, meter, tempo, bar lines, repeats `|: :|`, ties, broken rhythm, tuplets,
  chords, inline fields, multi-voice. Output: a flat `Tune` of `MidiEvent`s.
- **Timing stays drift-free**: durations as integer fractions; `curMs` carried as a
  float; only the *emitted* ms stamps are rounded. Events finalized by a stable sort on
  `(timeMs, priority)` so note-offs fire before note-ons at a shared time.
- The scheduler thread fires the pre-sorted events at `midiOut` at their ms deadlines
  (`timeBeginPeriod(1)` for 1 ms resolution); per-track held-note tables let one cue
  stop without cutting another.

Design: [../../../docs/gameaudio.md](../../../docs/gameaudio.md) (the **music/** section).
Reference: `E:\NewModula2\library\winrtmod\Abc.mod`.

Status: not built yet — design only.
