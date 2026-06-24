# demo.tcl — drive Jewels through a scripted game and narrate the score. No
# asserts: it's the "watch it play" mode. Start the game, run this, and watch the
# window while the script reports each step.
#   nanotcl projects/jewels/tests/demo.tcl

send "newgame"
puts "RASM Jewels — scripted demo"

# 1) a three-colour cascade: blue/green/red into columns 0,1,2 so a chain rips
#    through (reds clear, greens fall and match, blues fall and match)
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
puts "  cascade       -> score [lindex $r 0], cleared [lindex $r 4]"

# 2) a straight vertical triple of yellow
send "setpiece rcx=14 rdx=14 r8=14"
send "drop"
set r [regs]
puts "  yellow triple -> score [lindex $r 0], cleared [lindex $r 4]"

# 3) leave a couple of mixed jewels sitting in the well to look at
send "setpiece rcx=12 rdx=9 r8=10"
send "right"
send "right"
send "drop"
set r [regs]
puts "  parked a mixed stack; state [lindex $r 1]"

puts "demo done — the game is left free-running, go play it"
