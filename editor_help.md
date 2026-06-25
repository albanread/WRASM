# WRASM Studio — Editor Reference

Keys, mouse, and the automatic behaviors of the Studio editor (`studio.exe`),
as implemented in [crates/studio/src/bin/studio.rs](crates/studio/src/bin/studio.rs)
and [crates/studio/src/doc.rs](crates/studio/src/doc.rs).

The editor is a single text pane on the left, a knowledge **assistant** pane on
the right, and an **output** pane along the bottom; the right and bottom panes
can be collapsed (see [Mouse & panes](#mouse--panes)).

Tabs are soft — **two spaces**, never a `\t`.

---

## Files

| Key | Action |
|-----|--------|
| `Ctrl+N` | New (empty) document |
| `Ctrl+O` | Open… (file dialog) |
| `Ctrl+S` | Save |
| `Ctrl+Shift+S` | Save As… |

Menu-only: **File ▸ Open Recent** (last 9 files), **File ▸ Export Listing…**,
**File ▸ Exit**. Files are read and written with `\n` line endings (CRLF on open
is normalized to LF).

## Edit & history

| Key | Action |
|-----|--------|
| `Ctrl+Z` | Undo |
| `Ctrl+Shift+Z` / `Ctrl+Y` | Redo |
| `Ctrl+C` | Copy |
| `Ctrl+X` | Cut |
| `Ctrl+V` | Paste |
| `Ctrl+A` | Select all |

Typing is coalesced into a single undo step until you move the caret, so undo
walks back word-by-word rather than character-by-character. The undo history is
capped at 500 steps.

## Caret movement & selection

| Key | Action |
|-----|--------|
| `←` `→` `↑` `↓` | Move by character / line |
| `Ctrl+←` / `Ctrl+→` | Move by word |
| `Home` | **Smart Home** — first press → first non-blank column; second press → column 0 |
| `End` | End of line |
| `Ctrl+Home` / `Ctrl+End` | Top / bottom of the document |
| `PageUp` / `PageDown` | Move one page |
| `Shift +` *(any move above)* | Extend the selection instead of dropping it |

`Home`/`End` are line-scoped; `Ctrl+Home`/`Ctrl+End` jump to the document
extremes.

## Line & text editing

| Key | Action |
|-----|--------|
| `Enter` | New line, **auto-indented** (see below) |
| `Backspace` / `Delete` | Delete left / right |
| `Ctrl+Backspace` / `Ctrl+Delete` | Delete a word left / right |
| `Tab` | Insert two spaces; if text is selected, **indent** the selected lines |
| `Shift+Tab` | **Outdent** the current / selected lines |
| `Alt+↑` / `Alt+↓` | Move the current line up / down |
| `Ctrl+D` | Duplicate the current line |
| `Ctrl+Shift+K` | Delete the current line |
| `Ctrl+/` | Toggle `;` line comment on the selection |

## Code navigation & intelligence

| Key | Action |
|-----|--------|
| `Ctrl+F` | In-document **Find / Replace** bar |
| `F3` / `Shift+F3` | Next / previous match — works with the find bar closed too (seeds from the selection or word at the caret the first time) |
| `Ctrl+Shift+F` | Search the **Windows API knowledge base** (assistant pane) |
| `Ctrl+G` | **Go-to-label** palette — jump to a symbol in the document |
| `Ctrl+Click` | **Go to definition** of the equate / symbol under the cursor |

Autocomplete pops up as you type. While it is open:

| Key | Action |
|-----|--------|
| `↑` / `↓` | Move the highlight |
| `Tab` / `Enter` | Accept the highlighted candidate |
| `Esc` | Dismiss |
| `←` `→` `Home` `End` `PageUp` `PageDown` | Close the popup, then perform the move |

`Ctrl` shortcuts still work while the popup is open.

## Build, run & capture

| Key | Action |
|-----|--------|
| `F5` | **Build** the current source to a `.exe` |
| `F6` | **Run** the built program |
| `F12` | Save a PNG snapshot of the window (`studio_shot_<timestamp>.png` in the working dir) |

Build/run expand `.include` directives first; an include error is reported in
the output pane instead of assembling.

---

## Modal overlays

These open over the editor and capture most keystrokes until dismissed.

### Find / Replace bar — `Ctrl+F`

Seeds the search needle from the current selection (single line) or the word at
the caret. Matches update live and the view jumps to the nearest one.

| Key | Action |
|-----|--------|
| *type* | Edit the active field (live re-search + jump) |
| `Tab` | Switch between the **Find** and **Replace** fields |
| `Enter` | Next match (Find field) / replace the current match (Replace field) |
| `↑` / `↓` | Previous / next match |
| `F3` / `Shift+F3` | Next / previous match |
| `Esc` | Close the bar |

The bar also has clickable buttons: **▲ / ▼** (previous / next match), **Replace**
(replace the current match), **All** (replace every match), and **✕** (close).
Replace-all is button-only — there is no keyboard chord for it.

### Go-to-label palette — `Ctrl+G`

| Key | Action |
|-----|--------|
| *type* | Filter the label list |
| `↑` / `↓` | Move the selection |
| `Enter` | Jump to the selected label |
| `Esc` | Close |

A click on a row jumps there; a click anywhere else dismisses the palette.

### API knowledge search — `Ctrl+Shift+F` (or click the search field)

| Key | Action |
|-----|--------|
| *type* | Edit the query |
| `Enter` | Run the search (results in the assistant pane) |
| `Esc` | Close the search box |

---

## Mouse & panes

| Gesture | Action |
|---------|--------|
| Click (editor) | Place the caret |
| `Shift+Click` | Extend the selection to the click |
| `Ctrl+Click` | Go to the definition of the clicked equate / symbol |
| Double-click | Select the word |
| Triple-click | Select the line |
| Drag | Select a range of text |
| Click (assistant pane) | Focus the search box, or follow a card link |
| Drag a splitter band | Resize the assistant / output pane |
| Click a splitter toggle | Collapse or restore that pane (assistant on the right, output along the bottom) |

---

## Automatic behaviors

- **Auto-pairing.** Typing `[`, `(`, or `{` inserts the matching close and
  leaves the caret between them. `"` and `'` pair the same way *unless* the next
  character is alphanumeric (so apostrophes inside words are left alone). Typing
  a closing `]` `)` `}` `"` `'` when that character is already to the right just
  steps over it. Auto-pairing is skipped while text is selected.
- **Auto-indent on `Enter`.** The new line copies the previous line's leading
  whitespace, plus one extra level (two spaces) when the line opens a block —
  `proc`, `.if`, `.while`, `.repeat`, `.for`, `struct`, or `macro`.
- **Smart Home.** `Home` toggles between the first non-blank column and column 0.
- **Soft tabs.** `Tab` is two spaces; `Tab`/`Shift+Tab` over a selection
  indent/outdent in two-space steps.

---

## Menu map

| Menu | Items |
|------|-------|
| **File** | New, Open…, Open Recent ▸, Save, Save As…, Export Listing…, Exit |
| **Edit** | Undo, Redo, Cut, Copy, Paste, Select All |
| **Assembler** | Build .exe (`F5`), Run (`F6`) |
