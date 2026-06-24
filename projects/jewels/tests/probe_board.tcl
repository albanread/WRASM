# probe_board.tcl — assert the RENDERED column pixel by pixel (beats a snapshot).
# Drops one non-matching triple (blue/green/red) and checks each drawn jewel's
# centre pixel against the colour it should be. `probe x y` calls the library
# Pget and publishes the index in slot 5.
#   nanotcl projects/jewels/tests/probe_board.tcl
#
# Column 2 centre x = WELLX + 2*CELL + CELL/2 = 118 + 28 + 7 = 153.
# Row r centre y    = WELLY + r*CELL + CELL/2 = 16 + r*14 + 7.
#   row 9  -> 149   row 10 -> 163   row 11 -> 177

send "newgame"
send "setpiece rcx=9 rdx=10 r8=12"     ; # blue(9) / green(10) / red(12), top to bottom
send "drop"
set r [regs]                            ; # advance a frame so DrawBoard renders the stack

# probe each cell: send, let the read publish (slot 5), then read it
send "probe rcx=153 rdx=149"
set r [regs]
set top [lindex [regs] 5]
send "probe rcx=153 rdx=163"
set r [regs]
set mid [lindex [regs] 5]
send "probe rcx=153 rdx=177"
set r [regs]
set bot [lindex [regs] 5]

puts "rendered column 2 : top=$top mid=$mid bot=$bot (expect 9 10 12)"
assert [expr {$top == 9}]   "row 9 jewel should render blue (9), got $top"
assert [expr {$mid == 10}]  "row 10 jewel should render green (10), got $mid"
assert [expr {$bot == 12}]  "row 11 jewel should render red (12), got $bot"

# and the well interior between columns is black (index 0)
send "probe rcx=120 rdx=177"
set r [regs]
set gap [lindex [regs] 5]
assert [expr {$gap == 0}]   "well interior should be black (0), got $gap"

puts "probe_board: all checks passed"
