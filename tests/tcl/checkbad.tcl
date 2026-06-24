# checkbad.tcl — a deliberately failing hook, to prove the non-zero exit code.
# (The gems tick is positive, so asserting it is negative always fails.)
assert [expr {$rax < 0}] "rax should be negative (it never is) — expected failure"
