; abc_data.inc — the ABC parser's shared state, constants + key tables.
; Included by the abc.was shell inside .DATA. Not standalone.


; ---- the Tune event array ----
; 4096 events * 20 bytes = 81920 bytes.  Record: timeMs|status|chan|d1|d2 (u32).
; NOTE: the dup count must be a literal — the assembler does not fold `4096*20`.
tuneEv    BYTE 81920 dup(0)
tuneCount DWORD ?                     ; number of events emitted
tuneEndMs DWORD ?                     ; max timeMs after finalize
tuneBpm   DWORD ?                     ; effective bpm used

; ---- working parse state (globals; reset at the top of ParseTune) ----
txtPtr    QWORD ?                     ; the single-pass text cursor (char*)
curMs     real8 0.0                   ; running cursor in ms (carried as f64)
bpmVal    DWORD ?                     ; tempo, beats per minute
unitN     DWORD ?                     ; default note length numerator   (L:)
unitD     DWORD ?                     ; default note length denominator
tsN       DWORD ?                     ; meter numerator    (M:)
tsD       DWORD ?                     ; meter denominator
inBody    DWORD ?                     ; 0 = header, 1 = body (set by K:)

; ---- ornament / rhythm state (Sprint M-E) ----
; All reset at the top of ParseTune; each is INERT at its default (multipliers
; 1.0, counts 0, tiePend 0) so a tune with no ornaments behaves exactly as before.
brokenMul real8 1.0                  ; pending dur multiplier for the NEXT note (a>b)
tupCount  DWORD ?                    ; remaining notes inside an active tuplet
tupMul    real8 1.0                  ; the tuplet ratio (q/p) applied per tuplet note
tiePend   DWORD ?                    ; 1 = the previous note left a tie '-' open
tiePitch  DWORD ?                    ; the pitch of the note holding the open tie

; ParseNote ornament scratch (parked across calls; single-threaded .DATA).
pnDur     real8 0.0                  ; durWholes carried across RoundCurMs/EmitEvent
pnBrk     real8 0.0                  ; THIS note's broken-rhythm factor scratch

; ---- repeat / first-second-ending state (Sprint M-C) ----
; ONE level of repeat only (no nesting).  The repeat is replayed by JUMPING the
; text cursor back to repStart and letting pt_loop re-parse the section; curMs
; keeps advancing across the jump, so the replayed notes land at LATER
; timestamps (FinalizeTune sorts everything by time).  No event copying.
repStart  QWORD ?                     ; cursor just AFTER the last "|:" (jump target)
repArmed  DWORD ?                     ; 1 = the next ":|" will jump back to repStart
repPass   DWORD ?                     ; 1 = first pass, 2 = second pass (after a jump)
repVoice  DWORD ?                     ; which voice armed the repeat (don't cross voices)

; ---- per-voice state (MAXVOICE = 16, one per MIDI channel) ----
; Voices play SIMULTANEOUSLY: each has its OWN running time starting at 0.
; The global curMs is just the ACTIVE voice's working time; SwitchVoice
; saves curMs into the leaving voice's slot and loads the entering voice's.
; Index the f64 array by slot*8, the DWORD arrays by slot*4 (slot = number-1).
vCurMs    real8 16 dup(0.0)           ; per-voice running cursor in ms (f64)
vChan     DWORD 16 dup(0)             ; per-voice MIDI channel (default i)
vProgram  DWORD 16 dup(0)             ; per-voice program (instrument)
curVoice  DWORD 0                     ; active voice slot (0-based)
curChan   DWORD 0                     ; active channel (what EmitEvent stamps)

; Per-pitch-class state, indexed 0..6 for letters C D E F G A B.
keyAcc    DWORD 7 dup(0)              ; default accidental from the key signature
barAcc    DWORD 7 dup(0)             ; current bar accidental override (-2..+2)
barSet    DWORD 7 dup(0)             ; 1 if an explicit accidental active this bar

; semitone offset within an octave for pitch classes C D E F G A B (0..6)
semitab   DWORD 0, 2, 4, 5, 7, 9, 11

; pitch-class application order for sharps / flats (C=0 D=1 E=2 F=3 G=4 A=5 B=6)
sharpOrd  BYTE 3, 0, 4, 1, 5, 2, 6    ; F C G D A E B
flatOrd   BYTE 6, 2, 5, 1, 4, 0, 3    ; B E A D G C F

; base circle-of-fifths position per letter A..G (major); index = letter - 'A'.
;   A=3 B=5 C=0 D=2 E=4 F=-1 G=1
keyFifths BYTE 3, 5, 0, 2, 4, -1, 1

; ---- f64 constants ----
fHalf     real8 0.5
f60000    real8 60000.0
fZero     real8 0.0
fOne      real8 1.0                   ; broken/tuplet multiplier reset value
fDot5     real8 0.5                   ; broken rhythm: the SHORT side (x0.5)
fDot15    real8 1.5                   ; broken rhythm: the LONG side  (x1.5)
fGraceMs  real8 60.0                  ; grace-note fixed duration (ms) — see ParseGraces

; ---- finalize / file-write scratch ----
ftTemp    BYTE 20 dup(0)             ; scratch record for insertion sort
twWrote   DWORD ?                     ; bytes-written sink for WriteFile
twBytes   DWORD ?                     ; byteCount (via memory: invoke clobbers eax)

fTmp      real8 0.0                   ; AdvanceCurMs durWholes park
fDur      real8 0.0                   ; ParseNote durWholes park
fChDur    real8 0.0                   ; ParseChord durWholes park (across AdvanceCurMs)

; ParseChord pitch scratch: up to 8 simultaneous notes; one onset/offset for all.
; .DATA is safe — parsing is single-threaded (same pattern as ftTemp).
chPitch   DWORD 8 dup(0)              ; the chord's note pitches (cap 8, extras ignored)
