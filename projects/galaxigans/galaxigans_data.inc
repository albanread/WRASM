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
ENEMY_STRIDE   equ 40                        ; 10 i32 fields (see EN_* below)
EN_X           equ 0                         ; live SCREEN x (sway already folded in)
EN_Y           equ 4                         ; live SCREEN y
EN_ALIVE       equ 8
EN_STATE       equ 12                        ; ST_FORM / ST_DIVE
EN_HX          equ 16                        ; home slot x (pre-sway)
EN_HY          equ 20                        ; home slot y
EN_VX          equ 24                        ; peel velocity during the early dive
EN_DT          equ 28                        ; dive step counter
EN_ANIM        equ 32                        ; wing-flap phase counter
EN_TYPE        equ 36                        ; TYPE_BEE / TYPE_BUTTERFLY / TYPE_BOSS
ST_FORM        equ 0
ST_DIVE        equ 1
ENEMY_W        equ 16                        ; the new 16x16 shaded aliens (was 8x8)
ENEMY_H        equ 16
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

; ---- enemy bombs (divers drop them straight down) ----
BOMB_MAX       equ 6
BOMB_STRIDE    equ 12                        ; { x:i32, y:i32, active:i32 }
BOMB_DY        equ 4                         ; pixels/frame down
BOMB_PERIOD    equ 22                        ; frames between drops (the lowest diver drops)

; ---- sprite palette slot + colour indices (one shared 16-colour bank) ----
PAL_SLOT       equ 0
IDX_PLAYER     equ 1                         ; -> white
IDX_ENEMY      equ 2                         ; -> red
IDX_BULLET     equ 3                         ; -> yellow
IDX_BOMB       equ 4                         ; -> magenta
BEE_SLOT       equ 1                         ; the Bee's own 16-colour shaded palette (slot 1)
BUT_SLOT       equ 2                         ; Butterfly palette (slot 2)
BOSS_SLOT      equ 3                         ; Boss palette (slot 3)
TYPE_BEE       equ 0
TYPE_BUTTERFLY equ 1
TYPE_BOSS      equ 2

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
bombTimer  DWORD 22                           ; countdown to the next bomb drop

; pools (zero-initialised; ResetGame fills them). dup() needs a literal count.
enemies    BYTE  1280 dup(0)                  ; ENEMY_MAX * ENEMY_STRIDE (32*40)
bullets    BYTE  48  dup(0)                   ; BULLET_MAX * BULLET_STRIDE
bombs      BYTE  72  dup(0)                   ; BOMB_MAX * BOMB_STRIDE

; sprite frame ids (set by BuildSprAssets)
fPlayer    DWORD ?
fEnemy     DWORD ?                            ; Bee wings-up
fEnemyFlap DWORD ?                            ; Bee wings-down frame
fButterfly DWORD ?                            ; Butterfly (single frame)
fBoss      DWORD ?                            ; Boss (single frame)
fBullet    DWORD ?
fBomb      DWORD ?

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
; ---- BEE (Zako) 16x16, slot 1: blue body ramp (2-5), amber wing ramp (6-9), red eyes (A).
;      Every row is EXACTLY 16 chars (rows pack contiguously). ----
.balign 16
beeArt     BYTE "................"
           BYTE "......1221......"
           BYTE ".....12AA21....."
           BYTE ".....1AAAA1....."
           BYTE "98..12333321..89"
           BYTE "798.12344321.897"
           BYTE ".98.12355321.89."
           BYTE "..9.12344321.9.."
           BYTE "....12344321...."
           BYTE "....12333321...."
           BYTE ".....123321....."
           BYTE ".....122221....."
           BYTE "......1221......"
           BYTE "......1221......"
           BYTE "................"
           BYTE "................"
; BEE frame 1 — wings DOWN (the amber sweep dropped two rows for the flap)
beeArt2    BYTE "................"
           BYTE "......1221......"
           BYTE ".....12AA21....."
           BYTE ".....1AAAA1....."
           BYTE "....12333321...."
           BYTE "..9.12344321.9.."
           BYTE ".98.12355321.89."
           BYTE "798.12344321.897"
           BYTE "98..12344321..89"
           BYTE "....12333321...."
           BYTE ".....123321....."
           BYTE ".....122221....."
           BYTE "......1221......"
           BYTE "......1221......"
           BYTE "................"
           BYTE "................"
; BUTTERFLY (Goei) 16x16, slot 2: magenta wings (2-5), white body (6-8), cyan eyes (9).
.balign 16
butterflyArt BYTE "................"
           BYTE "......1661......"
           BYTE ".....169961....."
           BYTE "...2.16661.2...."
           BYTE "..23.16661.32..."
           BYTE ".234.16661.432.."
           BYTE "2345.16661.5432."
           BYTE "2345.16661.5432."
           BYTE ".234.16661.432.."
           BYTE "..23.16661.32..."
           BYTE "...2.16661.2...."
           BYTE "......1661......"
           BYTE "......1661......"
           BYTE "................"
           BYTE "................"
           BYTE "................"
; BOSS (flagship) 16x16, slot 3: green hull (2-5), purple horns (6-8), pink core (9), eyes (A).
.balign 16
bossArt    BYTE "................"
           BYTE ".7............7."
           BYTE ".78..........87."
           BYTE ".78.23444432.87."
           BYTE "78..23455432..87"
           BYTE "8...23499432...8"
           BYTE "....234A9A432..."
           BYTE "....23499432...."
           BYTE "....23444432...."
           BYTE "....23444432...."
           BYTE ".....234432....."
           BYTE ".....2.22.2....."
           BYTE "................"
           BYTE "................"
           BYTE "................"
           BYTE "................"
bulletArt  BYTE "33"
           BYTE "33"
           BYTE "33"
           BYTE "33"
           BYTE "33"
           BYTE "33"
bombArt    BYTE ".4."
           BYTE "444"
           BYTE "444"
           BYTE "444"
           BYTE ".4."

; scratch buffer for decoding a sprite frame (max 32x32 indexed pixels)
.balign 16
sprScratch BYTE  1024 dup(0)
