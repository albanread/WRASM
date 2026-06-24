module Sound
; sound_data.inc — sndBuf, f64 constants, the Effect block, WAV header, preset consts.
; Included by the sound.was shell inside .DATA. Not standalone.

.include "sinetab.inc"               ; sineTab: 1024 x f64, sin(2*pi*i/1024)

; PCM work buffer: 132300 frames x 2 channels = 264600 f64 = ~3.0 s of stereo at
; 44100 Hz.  A 500 ms Beep is 22050 frames (44100 f64), comfortably inside this.
.globl sndBuf                         ; exported: games call Play(sndBuf, sndFrames)
.globl sndFrames
sndBuf    real8  264600 dup(0)        ; interleaved L,R,L,R,... in [-1, 1]
sndFrames DWORD  ?                     ; frame count of the last render

; --- f64 constants (emitted as IEEE-754 bits) ---
cSR       real8  44100.0              ; sample rate
cHalf     real8  0.5                  ; output headroom scale
cOne      real8  1.0
cZero     real8  0.0
c1024     real8  1024.0              ; wavetable size (phase -> table index)
cI16Max   real8  32767.0            ; full-scale i16

; --- Sprint 2 constants (waveforms / render / presets) ---
c2Pi      real8  6.283185307179586   ; 2*pi  (radians per cycle)
cInv2Pi   real8  0.15915494309189535 ; 1/(2*pi)
cPi       real8  3.141592653589793   ; pi
cTwo      real8  2.0
cFour     real8  4.0
cNeg1     real8  -1.0
cHalfF    real8  0.5                  ; 0.5 literal (for saw/triangle "+0.5")
cInv16384 real8  0.00006103515625    ; 1/16384  (LCG signed scale)
cClampLo  real8  0.01                 ; min render duration (s)
cClampHi  real8  10.0                 ; max render duration (s)
cNorm     real8  0.9                  ; peak-normalize target
cTen      real8  10.0                 ; distortion drive scale (d = 1 + dist*10)
c27       real8  27.0                 ; tanh rational approx constants
c9        real8  9.0
lcgMul    DWORD  1103515245           ; LCG multiplier
lcgState  DWORD  12345                ; LCG state (seed 12345)

; ============================================================================
; Effect block (.DATA, single-threaded) — the render "recipe".  All fields are
; documented by byte offset so Render and the presets can address them directly.
; An Osc is 40 bytes: wave u32 + 4 pad (keep the f64s 8-aligned) then 4 x f64.
;
;   Osc @ base:  +0 wave(u32) +4 pad  +8 freq  +16 amp  +24 phase  +32 pulseWidth
;
; Effect field map (offsets from fxDur):
;   +0    fxDur        f64   duration seconds
;   +8    fxOsc        Osc[4]  (4 * 40 = 160 bytes)  -> +8 .. +167
;   +168  fxOscCount   u32
;   +172  (pad u32)
;   +176  fxAtk        f64   env attack  seconds
;   +184  fxDec        f64   env decay   seconds
;   +192  fxSus        f64   env sustain level 0..1
;   +200  fxRel        f64   env release seconds
;   +208  fxSweepStart f64   pitch glide start Hz (== End => off)
;   +216  fxSweepEnd   f64   pitch glide end   Hz
;   +224  fxNoiseMix   f64   0..1 dry/noise blend
;   +232  fxDistortion f64   0 = off
;   +240  fxEchoCount  u32
;   +244  (pad u32)
;   +248  fxEchoDelay  f64   echo tap spacing seconds
;   +256  fxEchoDecay  f64   per-tap decay (decay^e)
; ----------------------------------------------------------------------------
fxDur        real8  0.0
fxOsc:                                  ; Osc[0]
    DWORD 0
    DWORD 0
    real8 0.0, 0.0, 0.0, 0.0
    DWORD 0                             ; Osc[1]
    DWORD 0
    real8 0.0, 0.0, 0.0, 0.0
    DWORD 0                             ; Osc[2]
    DWORD 0
    real8 0.0, 0.0, 0.0, 0.0
    DWORD 0                             ; Osc[3]
    DWORD 0
    real8 0.0, 0.0, 0.0, 0.0
fxOscCount   DWORD  0
             DWORD  0                   ; pad
fxAtk        real8  0.01
fxDec        real8  0.1
fxSus        real8  0.7
fxRel        real8  0.2
fxSweepStart real8  0.0
fxSweepEnd   real8  0.0
fxNoiseMix   real8  0.0
fxDistortion real8  0.0
fxEchoCount  DWORD  0
             DWORD  0                   ; pad
fxEchoDelay  real8  0.0
fxEchoDecay  real8  0.0

; --- Render / echo / distortion scratch (single-threaded .DATA slots) ---
; Echo needs a dry snapshot of the signal the size of sndBuf (264600 f64).
sndEcho   real8  264600 dup(0)         ; dry-signal copy for the echo taps
rsaveX6   real8  0.0                   ; Render parks/restores callee-saved xmm here
rsaveX7   real8  0.0
rsaveX12  real8  0.0
rsaveX13  real8  0.0
rsaveX14  real8  0.0
rdriveD   real8  0.0                   ; distortion drive d
rdriveNum real8  0.0                   ; tanh(s*d) numerator
reDelayFr DWORD  0                      ; echo delay in frames
reTotal   DWORD  0                      ; total interleaved samples (sndFrames*2)
reOff     DWORD  0                      ; current tap sample offset

; --- preset literal constants (f64 bits; no f64 immediates in asm) ---
kCoinF0   real8  987.77                 ; Coin osc0 freq (B5)
kCoinA0   real8  0.5
kCoinF1   real8  1318.51                ; Coin osc1 freq (E6)
kCoinA1   real8  0.3
kCoinAtk  real8  0.01
kCoinDec  real8  0.1
kCoinSus  real8  0.3
kCoinRel  real8  0.15
kJumpS0   real8  300.0                  ; Jump sweep 300->600
kJumpS1   real8  600.0
kJumpAtk  real8  0.01
kJumpDec  real8  0.05
kJumpSus  real8  0.5
kJumpRel  real8  0.1
kZapS0    real8  1000.0                 ; Zap sweep 1000->100 + noise 0.2
kZapS1    real8  100.0
kZapNm    real8  0.2
kZapAtk   real8  0.01
kZapDec   real8  0.05
kZapSus   real8  0.3
kZapRel   real8  0.08
kExpF0    real8  58.0                   ; Explode osc0 sine
kExpA0    real8  0.95
kExpF1    real8  86.0                   ; Explode osc1 triangle
kExpA1    real8  0.28
kExpS0    real8  135.0                  ; Explode sweep 135->32
kExpS1    real8  32.0
kExpNm    real8  0.09                   ; clamp(0.06*1.5,0,0.12)=0.09 (size=1.5)
kExpDist  real8  0.08
kExpAtk   real8  0.0015
kExpDec   real8  0.14
kExpSus   real8  0.0
kExpRel   real8  0.10
kToneAmp  real8  0.75                   ; Tone osc amp
kTonePw   real8  0.5                    ; Tone default pulseWidth (pulse uses 0.25)
kTonePwP  real8  0.25
kToneAtk  real8  0.01
kToneDec  real8  0.05
kToneSus  real8  0.8
kToneRel  real8  0.1
kDfltPw   real8  0.5                    ; AddOsc default pulseWidth
fxDecDflt real8  0.1                    ; ClearEffect default env decay
fxSusDflt real8  0.7                    ; ClearEffect default env sustain
fxRelDflt real8  0.2                    ; ClearEffect default env release
cThousand real8  1000.0                 ; ms -> s divisor
presetDur real8  0.0                    ; scratch: preset duration (seconds)
toneFreq  real8  0.0                    ; scratch: Tone frequency
toneWave  DWORD  0                       ; scratch: Tone waveform id
kWahAmp   real8  0.45                   ; Wah: per-osc amp (two detuned sines beat -> warble)
kWahSus   real8  0.92                   ; Wah: high sustain so the warble holds
wahFreq2  real8  0.0                    ; scratch: Wah detuned (second) oscillator freq

; --- ADSR parameters (seconds / level) ---
cAtk      real8  0.01                 ; attack  time
cDec      real8  0.05                 ; decay   time
cSus      real8  0.7                  ; sustain level
cRel      real8  0.10                 ; release time

; --- WAV header (44 bytes). The two u32 size fields are patched before write:
;   offset 4  = riffSize = 36 + dataBytes
;   offset 40 = dataBytes = sndFrames * 2ch * 2bytes
; Layout: "RIFF" | u32 riffSize | "WAVE" | "fmt " | u32 16 | u16 1 | u16 2 |
;         u32 44100 | u32 176400 | u16 4 | u16 16 | "data" | u32 dataBytes
wavHdr:
    BYTE 0x52, 0x49, 0x46, 0x46            ; "RIFF"
    DWORD 0                                ; riffSize  (patched @ offset 4)
    BYTE 0x57, 0x41, 0x56, 0x45            ; "WAVE"
    BYTE 0x66, 0x6D, 0x74, 0x20            ; "fmt "
    DWORD 16                               ; fmt chunk size
    WORD  1                                ; PCM
    WORD  2                                ; channels
    DWORD SAMPLE_RATE                      ; sample rate
    DWORD AUDIO_BPS                        ; byte rate = 44100*2*2
    WORD  4                                ; block align = 2*2
    WORD  16                               ; bits per sample
    BYTE 0x64, 0x61, 0x74, 0x61            ; "data"
    DWORD 0                                ; dataBytes (patched @ offset 40)

; i16 staging buffer for streaming PCM out (one block = 4096 frames * 2ch = 8192
; samples = 16384 bytes).  WriteWav fills and flushes this in chunks.
wavStage  WORD   8192 dup(0)
wavWrote  DWORD  ?                     ; bytes-written sink for WriteFile
wavBytes  DWORD  ?                     ; byteCount for the current WriteFile block
                                      ; (passed via memory: `invoke` uses eax/rax as
                                      ; scratch for stack args, so a register arg
                                      ; computed into eax would be clobbered)

endmodule
