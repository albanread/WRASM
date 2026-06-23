# find_labels.tcl — exercises the go-to-label palette (Ctrl+G).
#
#   mkdir -p target/uitest
#   cargo run -p studio --bin studio -- --script crates/studio/scripts/find_labels.tcl
#
# Drives the palette on the starter buffer (labels: APPEND, banner, main) and
# checks open / narrow / jump / dismiss. Code labels render blue, data green.

set dir target/uitest
size 1100 720

# Ctrl+G opens it, listing every label.
key Ctrl+G
assert-eq [find-active] 1 "Ctrl+G opens the palette"
set n [find-count]
assert [expr {$n > 0}] "the buffer has labels"
screenshot $dir/find_open.png

# Typing narrows the list live.
type "main"
assert-eq [find-selected] main "the filter selects `main`"
assert-eq [find-count] 1 "only one label matches `main`"
screenshot $dir/find_narrowed.png

# Enter jumps to it and closes the palette.
key Enter
assert-eq [find-active] 0 "Enter closes the palette"
assert-eq [caret-col] 0 "the caret lands at the start of the label's line"

# Esc dismisses without moving.
set before [caret-row]
key Ctrl+G
type "zzznope"
assert-eq [find-count] 0 "a nonsense filter matches nothing"
key Escape
assert-eq [find-active] 0 "Esc closes the palette"
assert-eq [caret-row] $before "Esc does not move the caret"

puts "find_labels: ok"
