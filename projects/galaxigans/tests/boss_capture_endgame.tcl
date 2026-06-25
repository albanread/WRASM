# boss_capture_endgame.tcl — the capture boss must END the game when it takes the LAST pilot
# (no negative lives, no infinite play), and must RETREAT up off-screen after a successful
# abduction rather than hover behind.
#   nanotcl projects/galaxigans/tests/boss_capture_endgame.tcl
# slots: 0 score, 1 bossState, 2 abducting, 3 lives, 4 gameState, 5 bossY
# states: gameState 0 PLAYING / 1 GAMEOVER ;  bossState 0 IDLE / 5 BEAM / 6 RETURN

# ---- Bug 1: the boss taking the LAST pilot ends the game ----
send "playnow"
set R [regs]
send "caplast"                              ; # lives=1, a pilot ~2 ticks from the boss
set R [regs]
set R [regs]
set R [regs]
set R [regs]
set gs [lindex $R 4]
set lv [lindex $R 3]
puts "last-pilot capture: gameState=$gs lives=$lv"
assert [expr {$lv == 0}]  "lives must land on exactly 0, not corrupt (got $lv)"
assert [expr {$gs == 1}]  "last pilot taken -> GAMEOVER (gameState 1, got $gs)"

# ---- Bug 2: a non-final abduction -> boss retreats up and off the screen, then despawns ----
send "playnow"
set R [regs]
send "capkeep"                              ; # lives=3, same near-complete abduction
set R [regs]
set R [regs]
set R [regs]
set bs  [lindex $R 1]
set lv2 [lindex $R 3]
set by1 [lindex $R 5]
puts "non-final capture: bossState=$bs lives=$lv2 bossY=$by1"
assert [expr {$lv2 == 2}] "one pilot taken, two remain (got $lv2)"
assert [expr {$bs == 6}]  "boss should be RETREATING (BS_RETURN 6, got $bs)"

# let it fly off the top
for {set i 0} {$i < 70} {incr i} { set R [regs] }
set bs2 [lindex $R 1]
set by2 [lindex $R 5]
puts "after retreat: bossState=$bs2 bossY=$by2"
assert [expr {$by2 < $by1}] "boss must move UP off-screen (bossY $by2 < $by1)"
assert [expr {$bs2 == 0}]   "boss gone -> BS_IDLE 0 (got $bs2)"

puts "BOSS CAPTURE ENDGAME + RETREAT: VERIFIED"
