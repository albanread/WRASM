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

; ---- finalize / file-write scratch ----
ftTemp    BYTE 20 dup(0)             ; scratch record for insertion sort
twWrote   DWORD ?                     ; bytes-written sink for WriteFile
twBytes   DWORD ?                     ; byteCount (via memory: invoke clobbers eax)

fTmp      real8 0.0                   ; AdvanceCurMs durWholes park
fDur      real8 0.0                   ; ParseNote durWholes park
