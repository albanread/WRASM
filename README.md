# WRASM

### Powerful assistance that conceals nothing

![The WRASM studio — hand-written asm with its live machine-code bytes on the left, the WNDCLASSEXW struct card (offsets, field types, ready-to-insert frame) on the right](selfie.png)

A from-scratch, self-contained **x86-64 assembler for Windows** — and the
knowledge-driven IDE growing around it. No LLVM, no JIT, no external linker:
source text goes in, a running `.exe` comes out.

Four things make it different:

- **Byte-identical to LLVM-MC.** The encoder is validated against LLVM as a
  differential oracle across integer, SSE/SSE2, AVX/AVX2 (VEX) and AVX-512
  (EVEX). A frozen corpus of 5,109 goldens gates every build — with no LLVM
  present at test time.
- **It knows Windows.** A read-only knowledge layer over a ~165,000-symbol
  database (functions + parameter types, constants, enums, struct layouts with
  byte offsets, and COM IIDs, vtable slots, and method parameter types) means you
  write `invoke CreateFileW, …`, `sizeof(RECT)`, `RECT.right`, and
  `pDevice.CreateRenderTargetView(…)`, not magic numbers.
- **Structured asm you can trust — automate the tedium, show every byte.**
  Data-aware macros (`invoke`, `comcall`/`comobj`, `iid`, `struct` instances) and
  a declared-subroutine convention (`proc … endproc` with `uses`/`in`/`out` and an
  opt-in `frame`) expand to *visible* instructions — never hidden codegen. The
  contract then *checks* what you declared: a callee-saved register clobbered
  across a call, an `in`/`out` mismatch, a stack left off the aligned frame.
- **Self-contained output.** It emits COFF `.obj` files *and* complete PE `.exe`
  files with their own import directory and thunks — no `link.exe` required.

> Status: the core (source → `.exe`) is complete and byte-identical to LLVM-MC.
> The authoring layer — the COM macros, the `proc`/`frame` convention and its
> contract/clobber checks, float→`xmm` marshaling, `.include`, `.ASCIISTRING` —
> is in and unit-tested. A growing demo corpus exercises it end to end: GDI
> framebuffers, a D3D11 shader Mandelbrot with rubber-band zoom, a Direct2D
> particle fountain, and the start of a retro indexed-colour game canvas. The IDE
> serves cards, live checks and builds from one headless language thread.

## Workspace

| crate | what it is |
|-------|------------|
| **`rasm`** (root) | The x86-64 encoder (Intel-syntax text → bytes), plus the COFF `.obj` and self-contained PE `.exe` writers, the differential-test corpus, and the `rasm-as` CLI. |
| **`winkb`** | The knowledge layer: a read-only API over `windows_api.db` (search, resolve, function + COM-method signatures, struct layouts, interfaces, snippets, did-you-mean). `winkb` CLI. |
| **`was`** | The Windows assembler front-end: rewrites a thin, *transparent* superset of Intel asm into rasm text, then assembles. `invoke`/`comcall`/`comobj`/`iid`, `struct` instances, `Struct.field`, `sizeof(T)`, `proc`/`frame`, `.if`/`.while`, `.ASCIISTRING`, `.include` — all expanding to instructions you can see, with the contract checks on top. `was` CLI (`.obj`/`.exe`/`--check`). |
| **`ide`** | The assistant as *content*: turns a winkb query into renderable markdown cards (functions with arg→register marshaling, structs, COM methods, registers + ABI, instruction/flags, the `proc` convention, your own local symbols), and models the interactive insert frame. GUI-free and unit-tested. `ide-card` CLI. |
| **`studio`** | The IDE front-end: the [language thread](crates/studio/src/lang.rs) (all `!Sync` state on one worker, message-passed), the live-check/ghost-byte seam, caret cards, and autocomplete. `studio-repl` drives the whole stack from a terminal. |

## The authoring layer

Everything below lowers to plain, visible x86-64 — inspect it with `was … --emit-asm`.

- **`invoke F, a, b, …`** — Win64-ABI marshaling from the db signature (shadow
  space, arg→register, **float args to `xmm` automatically** from the param type).
- **COM, the data-aware way** — `comobj p : ID3D11Device` then
  `p.CreateRenderTargetView([rip+tex], 0, pRTV)`; vtable slot, struct offsets, and
  `iid ID3D11Texture2D` GUIDs all come from the db.
- **`struct LABEL TYPE … ends`** — a struct instance laid out at the db's byte
  offsets (`BufferDesc.Width = 1280`).
- **`proc NAME uses … in … out … [frame] … endproc`** — a declared subroutine:
  visible prologue/epilogue, and the contract is *checked*. `frame` reserves the
  shadow/alignment once so the calls inside go lean. The caller-side **clobber
  check** warns when a value in a volatile register is destroyed across a call.
- **`.ASCIISTRING … .ENDASCIISTRING`** — embed raw text (HLSL shader source, say)
  verbatim; **`.include "file"`** — compose a program from many files.

## Build & test

```sh
cargo build            # rasm + winkb + was + ide
cargo test -p rasm     # encoder unit tests + the 5,109-golden corpus gate
cargo test -p was      # the front-end: macros, proc contracts, the checks
```

`studio` additionally needs **WF66**'s `docpane` (the shared Direct2D /
DirectWrite render core) checked out at `../WF66` relative to this repo:

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
cargo run -p ide   --bin ide-card -- RECT        # a struct card
cargo run -p ide   --bin ide-card -- rcx         # register / ABI card
cargo run -p ide   --bin ide-card -- proc        # the subroutine convention
cargo run -p was   --bin was -- hi.was --check   # live checks (incl. the contracts)
```

## Demos

The `examples/` corpus is the assembler's proving ground — each a hand-written
`.was` you can build and run:

| demo | what it shows |
|---|---|
| `fbwin`, `mandel`, `julia`, `life`, `plasma`, `fire`, `tunnel`, `starfield`, `metaballs`, `rotozoomer` | CPU framebuffers blitted via GDI |
| `mandel_gpu` / `mandel_gpu_proc` | a D3D11 **shader** Mandelbrot (HLSL via `.ASCIISTRING` + `D3DCompile`) with rubber-band zoom; the `_proc` version is 512 bytes smaller via a `frame` proc |
| `d2d_balls` | a **Direct2D** fountain of spinning, translucent, outlined marbles (SSE physics, the COM macros, float→`xmm`) |
| `gamescanvas` | the start of a retro **indexed-colour game canvas** (320×200 palette framebuffer, 5×7 font via `.include`, palette cycling) |

## License

MIT.
