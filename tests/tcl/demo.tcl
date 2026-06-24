# nano-TCL demo — this script runs in the Rust tool (nanotcl). The `while` loop,
# `set`, and `expr` execute LOCALLY; only the substituted `Verb reg=val` lines
# cross the wire to the running gems app.
#
#   nanotcl demo.tcl      (with the gems window already running)

pause

# a row of six discs, colours 9..14, positioned by expr
set i 0
while {$i < 6} {
    Disc rcx=[expr {36 + $i * 42}] rdx=158 r8=8 r9=[expr {9 + $i}]
    set i [expr {$i + 1}]
}

# two sparkles via the app's own Star verb
Star rcx=160 rdx=42 r8=14
Star rcx=92  rdx=30 r8=14

# grab a frame so we can see the result
Snapshot
