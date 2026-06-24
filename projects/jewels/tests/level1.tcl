# level1.tcl — drive RASM Jewels over nano-TCL and assert the level-1 scoring rules.
#
#   1. start the game (dev build):  projects\jewels\jewels.exe
#   2. run this test:               nanotcl projects\jewels\tests\level1.tcl
#   (exit code 0 = all asserts passed, non-zero = a failure — CI-friendly)
#
# Uses the generic driver verbs:  send "<verb args>"  forwards a raw line;
# regs  takes a frame-sync snapshot and returns the 16 registers as a list.
# RASM Jewels publishes:  0=score  1=state  2=pieceCol  3=pieceRow  4=cleared.

# ---- 1. a vertical match: an all-red triple lines up 3 in its column -> 30 ----
send "newgame"
send "setpiece rcx=12 rdx=12 r8=12"
send "drop"
set r [regs]
set score  [lindex $r 0]
set clears [lindex $r 4]
puts "vertical match : score=$score clears=$clears"
assert [expr {$score == 30}]  "vertical 3 should score 30, got $score"
assert [expr {$clears == 3}]  "vertical 3 should clear 3, got $clears"

# ---- 2. horizontal match + chain: blue/green/red into columns 0,1,2.
#         row of reds clears, greens collapse + match, then blues -> 90 ----
send "newgame"
send "setpiece rcx=9 rdx=10 r8=12"
send "left"
send "left"
send "drop"
send "setpiece rcx=9 rdx=10 r8=12"
send "left"
send "drop"
send "setpiece rcx=9 rdx=10 r8=12"
send "drop"
set r [regs]
set score  [lindex $r 0]
set clears [lindex $r 4]
puts "horizontal+chain: score=$score clears=$clears"
assert [expr {$score == 90}]  "3-row chain should score 90, got $score"
assert [expr {$clears == 9}]  "3-row chain should clear 9, got $clears"

# ---- 3. no false match: three DIFFERENT colours stacked -> nothing clears ----
send "newgame"
send "setpiece rcx=9 rdx=10 r8=12"
send "drop"
set r [regs]
set score [lindex $r 0]
puts "no false match  : score=$score state=[lindex $r 1]"
assert [expr {$score == 0}]   "mixed column must not clear, got $score"

# ---- 4. game over: pack the spawn column (col 2) until a new piece can't fit ----
send "newgame"
send "setpiece rcx=9 rdx=10 r8=12"
send "drop"
send "setpiece rcx=9 rdx=10 r8=12"
send "drop"
send "setpiece rcx=9 rdx=10 r8=12"
send "drop"
send "setpiece rcx=9 rdx=10 r8=12"
send "drop"
set r [regs]
set st [lindex $r 1]
puts "game over      : state=$st (2 = over)"
assert [expr {$st == 2}]  "a full spawn column should be game over (state 2)"

puts "RASM Jewels level 1: all checks passed"
