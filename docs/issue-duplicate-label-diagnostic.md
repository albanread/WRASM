# Issue: Duplicate labels silently overwrite earlier definitions

> **RESOLVED.** Fixed in `src/rasm/assemble.rs` by a duplicate-label validation
> pass between parse (pass 1) and branch relaxation (pass 2): a second definition
> now fails with `line N: duplicate label \`x\` (first defined on line M)` instead
> of silently overwriting the first. Covered by the `duplicate_labels_are_rejected`
> unit test (standalone, inline `label: insn`, and cross-section forms).

## Summary

The assembler currently accepts duplicate label definitions in one assembled module. During layout, labels are collected into a `BTreeMap<String, (Sect, usize)>`; a later `insert` silently replaces the earlier definition. Any branch, call, or RIP-relative reference to that label then resolves to the last definition, not the nearest or first one.

This caused the latest BrickOut FX executable to access-violate during startup. The ABC parser defines `pu_loop` / `pu_done` in `ParseUInt`, while BrickOut FX's score-pop update code also defined `pu_loop` / `pu_done`. The later score-pop labels overwrote the parser labels, so `ParseUInt` jumped into the unrelated score-pop loop and crashed while parsing the soundtrack's `M:4/4` field.

## Reproduction

Minimal assembled input:

```asm
.text
.globl main
main:
    call first
    ret

first:
    jmp done
done:
    ret

second:
    jmp done
done:
    mov rax, [rip + missing_or_wrong_state]
    ret
```

Expected: assembly fails with a duplicate label diagnostic for `done`.

Actual: assembly succeeds, and both jumps to `done` resolve to the second `done` label.

## Root Cause

`src/rasm/assemble.rs` computes labels in `layout` like this:

```rust
if let Item::Label(n) = it {
    labels.insert(n.clone(), (sect, off));
}
```

Because `BTreeMap::insert` returns and discards the previous value, duplicate labels are not reported. The same map is used during relaxation and emission, so the overwrite affects branch sizing, displacement patching, and reloc decisions.

This is especially easy to trigger after `.include` expansion because all included files share one module-level label namespace unless they use generated or explicitly prefixed local labels.

## Proposed Fix

Add a duplicate-label validation pass before branch relaxation, while `item_line` is still available:

1. Walk `items` and `item_line` after pass 1.
2. Track `label -> first lowered line` in a `BTreeMap` or `HashMap`.
3. On a second definition, return an error such as:

```text
line 641: duplicate label `pu_loop` (first defined on line 125)
```

4. Reject duplicates across `.text` and `.data`, since symbol references resolve through one label namespace.
5. Keep `layout` as a pure layout helper, or change it to return `Result` and include line information. A separate validation pass is simpler and avoids disturbing relaxation.

For `was`, this will naturally flow through the existing `remap_assemble_error` path so the diagnostic can point back to the original `.was` line after include/lowering maps are applied.

## Suggested Test

Add a unit test in `src/rasm/assemble.rs`:

```rust
#[test]
fn duplicate_labels_are_rejected() {
    let err = assemble(".text\nfoo:\nret\nfoo:\nret\n").unwrap_err().to_string();
    assert!(err.contains("duplicate label `foo`"), "{err}");
}
```

Also add a test for an inline label form:

```rust
let err = assemble(".text\nfoo: ret\nfoo: ret\n").unwrap_err().to_string();
assert!(err.contains("duplicate label `foo`"), "{err}");
```

## Notes

This should remain an error, not a warning. In the current assembler model labels are module-scoped symbols, and duplicate acceptance changes program control flow silently.