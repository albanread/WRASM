# ui_demo.tcl — a tour of the studio IDE's TCL UI-scripting verbs.
#
# Run headlessly (no window needed — it renders offscreen):
#   mkdir -p target/uitest
#   cargo run -p studio --bin studio -- --script crates/studio/scripts/ui_demo.tcl
#
# Any `assert*` failure aborts with a non-zero exit, so these scripts double as
# UI regression tests. Full TCL is available (set/if/expr/foreach/proc/…).
#
# Verbs:
#   input    : type "text" · key Ctrl+S · caret ROW COL
#   pointer  : move X Y · click X Y · drag X1 Y1 X2 Y2          (DIP coords)
#   panes    : splitter col FRAC · splitter row PX · collapse PANE · expand PANE
#   files    : open FILE · new · size W H
#   capture  : screenshot PATH.png
#   read-back: text · line N · linecount · caret-row · caret-col · notice
#              split-frac · output-h · assistant-open · output-open · hover-split
#   asserts  : assert COND ?msg? · assert-eq ACTUAL EXPECTED ?msg?

set dir target/uitest

size 1100 720
screenshot $dir/demo_default.png

# Resize the editor | assistant split to 45%, then screenshot.
splitter col 0.45
assert-eq [split-frac] 0.4500
screenshot $dir/demo_split45.png

# Hover the splitter to light it up (0.45 * 1100 = 495).
move 495 250
assert-eq [hover-split] col
screenshot $dir/demo_hover.png

# Collapse each pane in turn.
collapse assistant
assert-eq [assistant-open] 0
collapse output
assert-eq [output-open] 0
screenshot $dir/demo_both_collapsed.png

# Bring them back at their default sizes.
expand assistant
expand output
assert-eq [assistant-open] 1
assert-eq [output-open] 1

# Type a line and confirm it landed.
caret 0 0
type "; driven by tcl"
assert-eq [caret-col] 15
screenshot $dir/demo_typed.png

puts "ui_demo: ok"
