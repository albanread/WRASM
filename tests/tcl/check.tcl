# check.tcl — an agent test hook. nanotcl runs this at every frame-sync sample
# with the game's registers exposed as TCL vars (rax, rcx, … and prev_rax, …).
#
#   nanotcl attach 2 6 check.tcl        # exit 0 if all asserts pass, 1 otherwise
#
# The gems app exposes: rax = tick, rcx = orbitX, rdx = orbitY.

# the frame clock must advance every sample
assert [expr {$rax > $prev_rax}] "tick must advance (rax=$rax prev=$prev_rax)"

# the ship must actually be moving (orbit changes)
assert [expr {$rcx != $prev_rcx || $rdx != $prev_rdx}] "ship orbit is stuck"

puts "sample $sample ok: tick=$rax orbit=($rcx,$rdx)"

# the hook can also drive the game: drop a marker pixel each frame
Pset rcx=8 rdx=8 r8=15
