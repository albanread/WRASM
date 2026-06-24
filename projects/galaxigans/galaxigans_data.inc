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
ENEMY_ROWS     equ 5
ENEMY_MAX      equ 40                        ; COLS*ROWS
ENEMY_STRIDE   equ 44                        ; 11 i32 fields (see EN_* below)
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
EN_BANK        equ 40                        ; -1 banking left / 0 level / +1 right (while diving)
ST_FORM        equ 0
ST_DIVE        equ 1
ST_RETURN      equ 2                         ; surviving diver gliding back from the top to its slot
RETURN_SPEED   equ 4                         ; px/frame descent during the return
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
BULLET_STRIDE  equ 16                        ; { x:i32, y:i32, active:i32, dx:i32 }
BUL_DX         equ 12                        ; dx field offset (horizontal drift from the lean)
BULLET_DY      equ 7                         ; pixels/frame up
BULLET_LEAN    equ 3                         ; bullet dx when the ship is banking L/R
FIRE_COOLDOWN  equ 8                         ; frames between shots
PLAYER_W       equ 24                        ; the 24x24 hero ship
SHIP_SLOT      equ 16                         ; the ship's own palette (high slot: aliens own 1..8)
RESPAWN_FRAMES equ 90                        ; death pause before the ship comes back

; ---- enemy bombs (divers drop them straight down) ----
BOMB_MAX       equ 6
BOMB_STRIDE    equ 12                        ; { x:i32, y:i32, active:i32 }
BOMB_DY        equ 4                         ; pixels/frame down
BOMB_PERIOD    equ 22                        ; frames between drops (the lowest diver drops)

; ---- bonus saucer (UFO flyby that drops bombs) ----
SAUCER_Y        equ 20                       ; drifts across the top band
SAUCER_W        equ 32
SAUCER_H        equ 16
SAUCER_VX       equ 2                         ; drift speed
SAUCER_PERIOD   equ 420                       ; frames between appearances
SAUCER_BOMB_GAP equ 28                        ; frames between its bomb drops
SAUCER_SLOT     equ 17                        ; the saucer's own palette (high slot)
SAUCER_SCORE    equ 200

; ---- game lifecycle states + the triumphal spiral dance ----
STATE_PLAYING  equ 0
STATE_GAMEOVER equ 1
STATE_INTRO    equ 2                         ; title screen, waiting for fire
STATE_WIN      equ 3                         ; stage clear
WIN_LEN        equ 180                       ; stage-clear pause before the next stage
DANCE_CX       equ 320                       ; orbit centre (640x360 middle)
DANCE_CY       equ 180
DANCE_R0       equ 95                        ; base orbit radius
DANCE_R1       equ 70                        ; breathing amplitude
ANGLE_STEP     equ 6                         ; per-enemy angle offset (pinwheel spread)
RAD_STEP       equ 10                        ; per-enemy radius phase
DANCE_LEN      equ 420                       ; frames the dance runs before the restart

; ---- sprite palette slot + colour indices (one shared 16-colour bank) ----
PAL_SLOT       equ 0
IDX_PLAYER     equ 1                         ; -> white
IDX_ENEMY      equ 2                         ; -> red
IDX_BULLET     equ 3                         ; -> yellow
IDX_BOMB       equ 4                         ; -> magenta
BEE_SLOT       equ 1                         ; the Bee's own 16-colour shaded palette (slot 1)
BUT_SLOT       equ 2                         ; Butterfly palette (slot 2)
BOSS_SLOT      equ 3                         ; Boss palette (slot 3)
WASP_SLOT      equ 4                         ; Wasp palette (slot 4)
DRONE_SLOT     equ 5                         ; Drone/Grunt palette (slot 5)
JELLY_SLOT     equ 6                         ; Jellyfish (slot 6 = TYPE_JELLY+1) — later waves
BEETLE_SLOT    equ 7                         ; Beetle (slot 7)
STING_SLOT     equ 8                         ; Stingray (slot 8)
TYPE_BEE       equ 0
TYPE_BUTTERFLY equ 1
TYPE_BOSS      equ 2
TYPE_WASP      equ 3
TYPE_DRONE     equ 4
TYPE_JELLY     equ 5                         ; the three new later-wave species:
TYPE_BEETLE    equ 6                         ;   slot = TYPE+1 (the DrawScene convention)
TYPE_STING     equ 7
NUM_TYPES      equ 8
NUM_THEMES     equ 4                          ; per-stage formation themes (rowTypes table)

.balign 16
.DATA
.balign 16

; window title
titleW     WCHAR "RASM — Galaxigans", 0

; player
playerX    DWORD 312                         ; ship x (top-left), centred-ish
playerLean DWORD 0                            ; -1 banking left / 0 level / +1 right
fireCool   DWORD 0                           ; frames until next shot allowed
pAlive     DWORD 1                           ; 0 while exploding/respawning
pDeathT    DWORD 0                           ; respawn countdown while dead
lives      DWORD 3
score      DWORD 0
scoreStr   BYTE  "SCORE 000000", 0
livesStr   BYTE  "LIVES 3", 0
gameOverStr BYTE "GAME OVER", 0
titleStr   BYTE  "G A L A X I G A N S", 0
startStr   BYTE  "PRESS FIRE TO START", 0
clearStr   BYTE  "STAGE CLEAR", 0
stageStr   BYTE  "STAGE 1", 0

; game state + the dance/win clocks
gameState  DWORD 2                            ; boots into STATE_INTRO
stage      DWORD 1
danceFrame DWORD 0
danceTimer DWORD 0
winTimer   DWORD 0

; formation sway + dive scheduling
formX      DWORD 0                           ; horizontal sway offset
formDir    DWORD 1                           ; +1 / -1
diveTimer  DWORD 60                           ; countdown to the next dive launch
diveScan   DWORD 0                            ; rotating start index for picking a diver
bombTimer  DWORD 22                           ; countdown to the next bomb drop

; per-stage difficulty (set by ResetGame from `stage`) — speed + bombs ramp each level
gDivePeriod  DWORD 60                          ; dive cadence (smaller = more dives)
gBombPeriod  DWORD 22                          ; formation bomb cadence
gDiveVX      DWORD 3                            ; diver homing/peel speed
gSaucerGap   DWORD 28                           ; saucer bomb cadence
gStageOff    DWORD 0                            ; (stage-1) mod 5 — rotates the per-row species

; bonus saucer state
saucerActive DWORD 0
saucerX      DWORD 0
saucerDir    DWORD -1                          ; flips each spawn (enter L<->R)
saucerAnim   DWORD 0
saucerSpawn  DWORD 120                         ; countdown to the first appearance
saucerBombT  DWORD 0
saucerSndT   DWORD 0                            ; countdown to re-trigger the looping wah-wah

; pools (zero-initialised; ResetGame fills them). dup() needs a literal count.
enemies    BYTE  1760 dup(0)                  ; ENEMY_MAX * ENEMY_STRIDE (40*44)
bullets    BYTE  64  dup(0)                   ; BULLET_MAX * BULLET_STRIDE (4*16)
bombs      BYTE  72  dup(0)                   ; BOMB_MAX * BOMB_STRIDE

; sprite frame ids (set by BuildSprAssets)
fPlayer    DWORD ?                            ; (legacy 8x8 placeholder, unused)
fPlayerC   DWORD ?                            ; hero ship: level / left-bank / right-bank
fPlayerL   DWORD ?
fPlayerR   DWORD ?
fEnemy     DWORD ?                            ; Bee wings-up
fEnemyFlap DWORD ?                            ; Bee wings-down frame
fButterfly DWORD ?                            ; Butterfly (single frame)
fBoss      DWORD ?                            ; Boss (single frame)
fWasp      DWORD ?                            ; Wasp (single frame)
fDrone     DWORD ?                            ; Drone/Grunt (single frame)
fSaucer    DWORD ?                            ; bonus saucer (2-frame)
fSaucer2   DWORD ?
fBullet    DWORD ?
fBomb      DWORD ?

; formation row -> alien type (one species per row)
; per-stage formation themes — each row of 5 = the species for grid rows 0..4 (top..bottom).
; ResetGame picks theme (stage-1) mod NUM_THEMES, so later waves bring in the new species.
.balign 16
rowTypes   DWORD TYPE_BOSS,  TYPE_BUTTERFLY, TYPE_WASP,   TYPE_DRONE,  TYPE_BEE     ; 1: classic
           DWORD TYPE_BOSS,  TYPE_STING,     TYPE_WASP,   TYPE_BEETLE, TYPE_BEE     ; 2: + ray/beetle
           DWORD TYPE_STING, TYPE_JELLY,     TYPE_BEETLE, TYPE_DRONE,  TYPE_BUTTERFLY ; 3: new-heavy
           DWORD TYPE_JELLY, TYPE_BOSS,      TYPE_STING,  TYPE_BEETLE, TYPE_WASP    ; 4: full mix
rowTypePtr QWORD 0                            ; -> the selected theme's 5 entries (set by ResetGame)

; per-type frame tables, indexed by EN_TYPE (0..7): upright + left/right bank (for divers)
.balign 16
fUp        DWORD 8 dup(0)
fBankL     DWORD 8 dup(0)
fBankR     DWORD 8 dup(0)

; ship palette (slot 6 indices 1..15) — steel-blue hull, cyan cockpit, orange engine glow
.balign 16
shipPal    DWORD 0x00182840, 0x003A5070, 0x00567088, 0x007890A8, 0x00A8C0D8, 0x00FFFFFF
           DWORD 0x00184868, 0x0030C0FF, 0x00B0F0FF, 0x00FF6010, 0x00FFA020, 0x00FFE040
           DWORD 0x00A8C0D8, 0x0030C0FF, 0x00FFFFFF

; saucer palette (slot 7): silver hull (2-5), white (6), cyan dome (7-9), rim lights (A-C)
.balign 16
saucerPal  DWORD 0x00303840, 0x00505860, 0x00707880, 0x00909AA4, 0x00C0C8D0, 0x00FFFFFF
           DWORD 0x002080B0, 0x0040C8FF, 0x00C0F0FF, 0x00682018, 0x00FFE040, 0x00FF7818
           DWORD 0x00C0C8D0, 0x0040C8FF, 0x00FFFFFF

; per-stage saucer dome tint (indices 7,8,9) — a different-looking saucer each level
.balign 16
saucerDome DWORD 0x002080B0, 0x0040C8FF, 0x00C0F0FF    ; cyan   (stage 1)
           DWORD 0x00B03020, 0x00FF6038, 0x00FFC0A0    ; red    (stage 2)
           DWORD 0x0020A030, 0x0050E060, 0x00C0FFC0    ; green  (stage 3)
           DWORD 0x007030B0, 0x00B060F0, 0x00E0C0FF    ; violet (stage 4)
           DWORD 0x00B08010, 0x00FFC828, 0x00FFF0A0    ; gold   (stage 5)

; new-species palettes — indices 1..10, "shades of a hue" (dark outline -> bright highlight)
.balign 16
jellyPal   DWORD 0x00301840, 0x00482860, 0x00603880, 0x007850A0, 0x009068C0   ; jellyfish: purple
           DWORD 0x00A884D8, 0x00C0A0E8, 0x00D8C0F0, 0x0070E0FF, 0x00FFFFFF    ;   body -> cyan glow
.balign 16
beetlePal  DWORD 0x00103018, 0x00184828, 0x00206038, 0x00308048, 0x0040A058   ; beetle: emerald
           DWORD 0x0058C068, 0x0078D880, 0x00A0E8A0, 0x00D0F0C0, 0x00FFFFFF    ;   carapace
.balign 16
stingPal   DWORD 0x00102838, 0x00184050, 0x00205868, 0x00287888, 0x003898A8   ; stingray: steel-teal
           DWORD 0x0048B8C8, 0x0068D0E0, 0x0098E8F0, 0x00C8F4FF, 0x00FFFFFF

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
; WASP (weaver) 16x16, slot 4: teal body (2-5), lime wings (6-8), yellow stinger (9), magenta eyes (A).
.balign 16
waspArt    BYTE "................"
           BYTE "......1AA1......"
           BYTE ".....123321....."
           BYTE ".8...123321...8."
           BYTE ".78.12333321.87."
           BYTE "678.12344321.876"
           BYTE ".78.12344321.87."
           BYTE "..8.123321.8...."
           BYTE "....123321......"
           BYTE ".....12321......"
           BYTE "......121......."
           BYTE "......121......."
           BYTE ".......9........"
           BYTE ".......9........"
           BYTE "................"
           BYTE "................"
; DRONE (crystal) 16x16, slot 5: grey hull ramp (2-5), cyan glow core (6-8), red sensor (9).
.balign 16
droneArt   BYTE "................"
           BYTE ".......11......."
           BYTE "......1221......"
           BYTE ".....123321....."
           BYTE "....12344321...."
           BYTE "...1234554321..."
           BYTE "..123457754321.."
           BYTE ".12345688654321."
           BYTE ".12345689654321."
           BYTE "..123457754321.."
           BYTE "...1234554321..."
           BYTE "....12344321...."
           BYTE ".....123321....."
           BYTE "......1221......"
           BYTE ".......11......."
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
