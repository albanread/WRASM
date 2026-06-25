# capture_boss.tcl — assert the capture-boss RESCUE: shooting the boss while it is abducting a
# pilot releases the pilot (saved) and kills the boss for a bonus, with no life lost.
#   nanotcl projects/galaxigans/tests/capture_boss.tcl
# slots: 0 score, 1 bossState, 2 abducting, 3 lives, 4 gameState

send "playnow"                              ; # fresh PLAYING game
set R [regs]
send "bossarm"                              ; # force the boss into BEAM with a pilot rising
set R [regs]
set ab1 [lindex $R 2]
set lv1 [lindex $R 3]
assert [expr {$ab1 == 1}] "after bossarm, abducting should be 1 (got $ab1)"

send "hitboss"                              ; # put a bullet inside the boss
set R [regs]                                ; # CheckBossHit fires
set R [regs]                                ; # state settles + publishes
set ab2 [lindex $R 2]
set sc  [lindex $R 0]
set lv2 [lindex $R 3]
puts "rescue: abducting=$ab2 score=$sc lives=$lv2 (was $lv1)"
assert [expr {$ab2 == 2}]    "after hitboss, abducting should be 2 = released (got $ab2)"
assert [expr {$sc >= 500}]   "boss bonus should be +500 (got $sc)"
assert [expr {$lv2 == $lv1}] "lives must be unchanged — pilot saved (got $lv2 vs $lv1)"

puts "CAPTURE-BOSS RESCUE: VERIFIED"
