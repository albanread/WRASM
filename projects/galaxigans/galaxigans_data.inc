; galaxigans_data.inc — all game state (.DATA). The shell includes this first so every
; fragment sees the pools/equates. Shared .DATA/equate names live in ONE global namespace
; (the module wrappers only privatise CODE labels), so these stay unique. `.balign 16`
; guards the block (data-alignment trap).

; ---- field / layout (640x360 GPU logical space; read CanvasW/CanvasH for real bounds) ----
FIELD_W        equ 640
FIELD_H        equ 360
PLAYER_Y       equ 330                       ; ship sits near the bottom

; ---- enemy formation: an 8x4 grid (32 slots) ----
ENEMY_COLS     equ 8
ENEMY_ROWS     equ 4
ENEMY_MAX      equ 32                        ; COLS*ROWS
ENEMY_STRIDE   equ 32                        ; 8 i32 fields (see EN_* below)
EN_X           equ 0                         ; live SCREEN x (sway already folded in)
EN_Y           equ 4                         ; live SCREEN y
EN_ALIVE       equ 8
EN_STATE       equ 12                        ; ST_FORM / ST_DIVE
EN_HX          equ 16                        ; home slot x (pre-sway)
EN_HY          equ 20                        ; home slot y
EN_VX          equ 24                        ; peel velocity during the early dive
EN_DT          equ 28                        ; dive step counter
ST_FORM        equ 0
ST_DIVE        equ 1
DIVE_PERIOD    equ 60                        ; frames between dive launches
DIVE_VX        equ 3                         ; homing x-speed once a diver commits
DIVE_PEEL      equ 12                        ; steps of outward peel before homing
COL_GAP        equ 56
ROW_GAP        equ 34
GRID_X0        equ 96                        ; top-left of the formation
GRID_Y0        equ 56

; ---- player bullet pool (4 in flight, Galaga-style) ----
BULLET_MAX     equ 4
BULLET_STRIDE  equ 12                        ; { x:i32, y:i32, active:i32 }
BULLET_DY      equ 7                         ; pixels/frame up
FIRE_COOLDOWN  equ 8                         ; frames between shots
RESPAWN_FRAMES equ 90                        ; death pause before the ship comes back

; ---- sprite palette slot + colour indices (one shared 16-colour bank) ----
PAL_SLOT       equ 0
IDX_PLAYER     equ 1                         ; -> white
IDX_ENEMY      equ 2                         ; -> red
IDX_BULLET     equ 3                         ; -> yellow

.balign 16
.DATA
.balign 16

; window title
titleW     WCHAR "RASM — Galaxigans", 0

; player
playerX    DWORD 312                         ; ship x (top-left), centred-ish
fireCool   DWORD 0                           ; frames until next shot allowed
pAlive     DWORD 1                           ; 0 while exploding/respawning
pDeathT    DWORD 0                           ; respawn countdown while dead
lives      DWORD 3
score      DWORD 0
scoreStr   BYTE  "SCORE 000000", 0
livesStr   BYTE  "LIVES 3", 0

; formation sway + dive scheduling
formX      DWORD 0                           ; horizontal sway offset
formDir    DWORD 1                           ; +1 / -1
diveTimer  DWORD 60                           ; countdown to the next dive launch
diveScan   DWORD 0                            ; rotating start index for picking a diver

; pools (zero-initialised; ResetGame fills them). dup() needs a literal count.
enemies    BYTE  1024 dup(0)                  ; ENEMY_MAX * ENEMY_STRIDE (32*32)
bullets    BYTE  48  dup(0)                   ; BULLET_MAX * BULLET_STRIDE

; sprite frame ids (set by BuildSprAssets)
fPlayer    DWORD ?
fEnemy     DWORD ?
fBullet    DWORD ?

; ---- sprite art: hex-digit bitmaps row-major (digit = palette index, '.'=0=clear).
;      Each BYTE "row" packs contiguously, so the label spans the whole w*h grid. ----
.balign 16
playerArt  BYTE "...11..."
           BYTE "...11..."
           BYTE "..1111.."
           BYTE ".111111."
           BYTE "11111111"
           BYTE "11111111"
           BYTE "1.1111.1"
           BYTE "1..11..1"
enemyArt   BYTE "2......2"
           BYTE ".2....2."
           BYTE ".222222."
           BYTE "22122122"
           BYTE "22222222"
           BYTE ".222222."
           BYTE ".2.22.2."
           BYTE "2.2..2.2"
bulletArt  BYTE "33"
           BYTE "33"
           BYTE "33"
           BYTE "33"
           BYTE "33"
           BYTE "33"

; scratch buffer for decoding a sprite frame (max 32x32 indexed pixels)
.balign 16
sprScratch BYTE  1024 dup(0)
