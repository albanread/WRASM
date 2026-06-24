; jewels_data.inc — constants + state for the Columns-style jewels game.
; A 6x12 well; a falling triple of jewels; line up 3+ of a colour (horizontal or
; vertical) to clear them, the stack collapses, chains score more. Level 1.

; ---- board geometry (pixels; framebuffer is 320x200 indexed) ----
COLS     equ 6
ROWS     equ 12
CELL     equ 14                 ; pixels per cell
WELLX    equ 118                ; well left edge  ((320 - COLS*CELL)/2 = 118)
WELLY    equ 16                 ; well top edge   ((200 - ROWS*CELL)/2 = 16)
JEWR     equ 6                  ; jewel radius (fits inside a 14px cell)
; combined constants (arithmetic only folds in an `equ`, not in an operand)
WELLW    equ COLS*CELL          ; 84  — well interior width
WELLH    equ ROWS*CELL          ; 168 — well interior height
WELLBX   equ WELLX-2            ; 116 — border x
WELLBY   equ WELLY-2            ; 14  — border y
WELLBW   equ COLS*CELL+4        ; 88  — border width
WELLBH   equ ROWS*CELL+4        ; 172 — border height
JCXOFF   equ WELLX + CELL/2     ; 125 — jewel centre-x base
JCYOFF   equ WELLY + CELL/2     ; 23  — jewel centre-y base
ROWSM1   equ ROWS-1             ; 11  — bottom row index

; ---- timing (level 1) ----
GRAV1    equ 30                 ; frames between gravity steps (~0.5s)
GRAVFAST equ 3                  ; soft-drop interval while DOWN is held

; ---- game states ----
ST_FALL  equ 0                  ; a triple is falling, player has control
ST_OVER  equ 2                  ; spawn blocked -> game over
ST_TITLE equ 3                  ; attract screen, waiting for FIRE to start

; ---- palette indices (default CGA-16) ----
PAL_BG   equ 8                  ; dark grey background
PAL_WELL equ 0                  ; black well interior
PAL_BORD equ 7                  ; light grey well border
PAL_TEXT equ 15                 ; white score text

.DATA
.balign 16
jwGrid     BYTE 72 dup(0)       ; [row*COLS + col]; 0 = empty, else a palette index
jwPal      BYTE 9, 10, 12, 14   ; the 4 jewel colours: blue, green, red, yellow
.balign 8
jwPieceCol DWORD 0              ; column of the falling triple
jwPieceRow DWORD 0              ; row of the TOP jewel of the triple
jwP0       DWORD 0              ; top colour    (palette index)
jwP1       DWORD 0              ; middle colour
jwP2       DWORD 0              ; bottom colour
jwScore    DWORD 0
jwState    DWORD 0             ; ST_FALL / ST_OVER
jwGrav     DWORD 0             ; gravity frame counter
jwSeed     DWORD 305419896     ; 0x12345678 — LCG seed
jwClears   DWORD 0             ; total jewels cleared (for the introspector ping)
jwNext0    DWORD 0             ; forced next-piece colours (deterministic TCL tests);
jwNext1    DWORD 0             ;   0,0,0 = pick at random
jwNext2    DWORD 0
jwMarked   BYTE 72 dup(0)      ; match-scan scratch (1 = part of a >=3 run)
jwLevel    DWORD 1             ; difficulty level (gravity speeds up with it)
jwProbe    DWORD 0             ; last `probe x y` pixel read (rendered-board assertions)
jwScoreStr BYTE "SCORE 00000", 0
jwLevelStr BYTE "LEVEL 00", 0
jwTitleStr BYTE "RASM JEWELS", 0
jwStartStr BYTE "PRESS FIRE", 0
jwOverStr  BYTE "GAME OVER", 0
.balign 16
