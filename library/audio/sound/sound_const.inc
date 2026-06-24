; sound_const.inc — named audio-format constants for the sound synth + mixer.
; Included at the top of sound.was (before sound_data.inc), so every program that
; reuses the sound library — even standalone tests that don't include canvas —
; has these in scope. `equ` equates fold to the same literals (byte-neutral).
;
; 44.1 kHz / 16-bit / stereo PCM throughout. AUDIO_BPS is the WAVEFORMATEX
; nAvgBytesPerSec and the WAV header's byte rate.

SAMPLE_RATE equ 44100                          ; frames per second
AUDIO_CH    equ 2                              ; channels (stereo)
AUDIO_BITS  equ 16                             ; bits per sample
AUDIO_BPS   equ SAMPLE_RATE * AUDIO_CH * (AUDIO_BITS / 8)   ; 176400 = nAvgBytesPerSec
