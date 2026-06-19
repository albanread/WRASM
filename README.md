# WRASM

A from-scratch, self-contained **x86-64 assembler for Windows** — and the
knowledge-driven IDE growing around it. No LLVM, no JIT, no external linker:
source text goes in, a running `.exe` comes out.

Three things make it different:

- **Byte-identical to LLVM-MC.** The encoder is validated against LLVM as a
  differential oracle across integer, SSE/SSE2, AVX/AVX2 (VEX) and AVX-512
  (EVEX). A frozen corpus of 5,109 goldens gates every build — with no LLVM
  present at test time.
- **It knows Windows.** A read-only knowledge layer over a ~165,000-symbol
  database (functions, constants, enums, struct layouts with byte offsets, and
  COM IIDs + vtable slots) means you write `invoke CreateFileW, …` and
  `sizeof(RECT)` and `RECT.right`, not magic numbers.
- **Self-contained output.** It emits COFF `.obj` files *and* complete PE
  `.exe` files with their own import directory and thunks — no `link.exe`
  required.

> Status: the core (source → `.exe`) is complete. The IDE backbone — a
> language thread serving cards, checks and assemblies — is in, fully
> headless-tested. The Direct2D front-end is in progress.

## Workspace

| crate | what it is |
|-------|------------|
| **`rasm`** (root) | The x86-64 encoder (Intel-syntax text → bytes), plus the COFF `.obj` and self-contained PE `.exe` writers, the differential-test corpus, and the `rasm-as` CLI. |
| **`winkb`** | The knowledge layer: a read-only API over `windows_api.db` (search, resolve, function signatures, struct layouts, COM interfaces, snippets, did-you-mean). `winkb` CLI. |
| **`was`** | The Windows assembler front-end: rewrites a thin superset of Intel asm — `invoke`, bare constants, `Struct.field`, `sizeof(T)` — into rasm text, then assembles. `was` CLI (`.obj`/`.exe`/`--check`). |
| **`ide`** | The assistant as *content*: turns a winkb query into renderable markdown cards, and models the interactive insert frame (`{{field}}`/`{{select}}`). GUI-free and unit-tested. `ide-card` CLI. |
| **`studio`** | The IDE front-end: the [language thread](crates/studio/src/lang.rs) (the Corman Lisp / WF66 pattern — all `!Sync` state on one worker, message-passed) and the docpane render seam. `studio-repl` drives the whole stack from a terminal. |

## Build & test

The core is self-contained:

```sh
cargo build            # rasm + winkb + was + ide
cargo test -p rasm     # encoder unit tests + the 5,109-golden corpus gate
```

`studio` additionally needs **WF66**'s `docpane` (the shared Direct2D /
DirectWrite render core) checked out at `../WF66` relative to this repo, since
it is consumed as a path dependency while the IDE takes shape:

```sh
cargo build --workspace      # everything, including studio (needs ../WF66)
cargo test  --workspace
```

### The knowledge database

`winkb` reads `windows_api.db` — a SQLite database derived from the Win32
metadata (not committed here; it's large). Point `winkb`/`was`/`studio` at it
with `$WINKB_DB`, defaulting to `E:\windows_api\windows_api.db`.

## Try it

```sh
# A self-contained exe with no toolchain — exits 42:
printf '.globl main\nmain:\n  invoke ExitProcess, 42\n  ret\n' > hi.was
cargo run -p was -- hi.was -o hi.exe && ./hi.exe; echo $?

# Ask the knowledge base things:
cargo run -p winkb --bin winkb -- show CreateFileW
cargo run -p ide   --bin ide-card -- RECT
cargo run -p was   --bin was -- hi.was --check

# Drive the language thread (needs ../WF66 for studio):
cargo run -p studio --bin studio-repl
#   was> CreateFileW          # a function card
#   was> :frame CreateFileW   # the fill-in-the-blanks invoke
#   was> :exe hi.was hi.exe   # build a self-contained exe
```

## License

MIT.
