# moves.tcl — the falling column clamps at the well walls.
# Shows: driving input verbs + reading a published slot (pieceCol = slot 2).
#   nanotcl projects/jewels/tests/moves.tcl

send "newgame"

# from the spawn column (2), three lefts can only reach column 0
send "left"
send "left"
send "left"
set c [lindex [regs] 2]
puts "left x3 from col 2 : pieceCol=$c (expect 0, clamped at the wall)"
assert [expr {$c == 0}]  "3x left from col 2 should clamp at 0, got $c"

# five rights from column 0 reach the last column (5) and stop
send "right"
send "right"
send "right"
send "right"
send "right"
set c [lindex [regs] 2]
puts "right x5 from col 0: pieceCol=$c (expect 5, clamped at the wall)"
assert [expr {$c == 5}]  "5x right from col 0 should clamp at 5, got $c"

# one more right cannot pass the wall
send "right"
set c [lindex [regs] 2]
assert [expr {$c == 5}]  "right at col 5 must not pass the wall, got $c"

puts "moves: all checks passed"
