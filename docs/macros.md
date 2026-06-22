# Macros — the WAS Front-End

The `was` front-end extends Intel-syntax assembly with macros that expand to
visible instructions before encoding. `--emit-asm` always shows the expanded
form — there is no hidden codegen.

## invoke

Calls a Windows API function with Win64 ABI marshaling.

```asm
invoke CreateFileW, lpFileName, GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0
```

Expansion sequence:

```mermaid
sequenceDiagram
    participant SRC as Source
    participant WAS as was macro
    participant KB as Knowledge DB
    participant ASM as Emitted asm

    SRC->>WAS: invoke CreateFileW, args...
    WAS->>KB: look up CreateFileW signature
    KB-->>WAS: (HANDLE, LPCWSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD, DWORD, HANDLE)
    WAS->>ASM: mov rcx, arg1
    WAS->>ASM: mov edx, arg2
    WAS->>ASM: mov r8d, arg3
    WAS->>ASM: mov r9d, arg4
    WAS->>ASM: push arg8 (stack args)
    WAS->>ASM: sub rsp, 32 (shadow space)
    WAS->>ASM: call qword [rip+CreateFileW]
    WAS->>ASM: add rsp, 32+stackargs
```

Win64 ABI rules applied automatically:

- First four integer/pointer args → `rcx`, `rdx`, `r8`, `r9`
- Float args → `xmm0`–`xmm3` (detected from parameter type in knowledge base)
- Additional args pushed right-to-left
- 32-byte shadow space reserved and cleaned
- Stack aligned to 16 bytes at the call site

## proc / endproc

Declares a subroutine with an explicit register contract.

```asm
proc  DrawLine  uses rsi rdi rbx  in rcx rdx r8 r9
    ; rcx = x1, rdx = y1, r8 = x2, r9 = y2
    ; rbx, rsi, rdi used internally — saved/restored automatically
    mov  rsi, rcx
    ...
endproc
```

### Clause meanings

| Clause | What it declares |
|--------|-----------------|
| `uses r1 r2 …` | Callee-saved registers modified in the body (push/pop emitted) |
| `in r1 r2 …` | Registers that carry input — must be set by caller |
| `out r1 r2 …` | Registers that carry output — must be written in the body |
| `frame` | One-time stack alignment; all `call`s inside use the aligned frame |

### Stack frame layout (with `frame`)

```mermaid
flowchart TD
    RSP0["RSP on entry"] --> RETADDR["return address (8 bytes)"]
    RETADDR --> SAVED["callee-saved regs (uses list)"]
    SAVED --> PAD["alignment padding if needed"]
    PAD --> SHADOW["32-byte shadow space"]
    SHADOW --> RSP1["RSP at call sites inside proc"]
```

Without `frame`, each inner `call` inside the proc must manage its own
alignment. With `frame`, the proc's prologue aligns once; inner calls sit
at the correct 16-byte boundary automatically. `--check` validates this.

### Proc contract checking

```
was myprog.was --check
```

| Diagnostic | Triggered when |
|------------|---------------|
| `register modified but not in uses:` | Callee-saved reg written without declaration |
| `clobber: value in %rcx lost across call` | Volatile reg overwritten before save |
| `register declared in: but never read` | Declared input never used |
| `register declared out: but never written` | Promised output missing |

## comobj / comcall

Declare a COM interface variable and call its methods.

```asm
comobj  pDevice : ID3D11Device

; Call CreateRenderTargetView
comcall pDevice.CreateRenderTargetView(pTexture, 0, pRTV)
```

Expansion:

```mermaid
sequenceDiagram
    participant SRC as Source
    participant WAS as was macro
    participant KB as Knowledge DB
    participant ASM as Emitted asm

    SRC->>WAS: comcall pDevice.CreateRenderTargetView(...)
    WAS->>KB: vtable slot of CreateRenderTargetView in ID3D11Device
    KB-->>WAS: slot 27, signature (pResource, pDesc, ppRTView) -> HRESULT
    WAS->>ASM: mov rcx, [rip+pDevice] (this pointer)
    WAS->>ASM: marshal pResource to rdx
    WAS->>ASM: marshal pDesc to r8
    WAS->>ASM: marshal ppRTView to r9
    WAS->>ASM: mov rax, [rcx]  (load vtable pointer)
    WAS->>ASM: call qword [rax + 27*8]
```

Float parameters are automatically routed to `xmm` registers based on the
method's signature in the knowledge base.

## struct

Declare a struct instance with named-field initializers:

```asm
struct  wndClass  WNDCLASSEXW  cbSize=sizeof(WNDCLASSEXW), style=CS_HREDRAW, lpfnWndProc=WndProc, ends
```

Fields not listed are zero-initialized. `ends` closes the declaration.
Field names resolve to byte offsets via the knowledge base:

```asm
; WNDCLASSEXW.cbSize is at offset 0, .style at offset 4, etc.
mov dword [rbx + WNDCLASSEXW.cbSize], 48
```

`sizeof(Type)` returns the size in bytes of any type in the knowledge base:

```asm
mov eax, sizeof(RECT)      ; = 16
mov eax, sizeof(POINT)     ; = 8
```

## iid

Embed a COM interface GUID as a 16-byte literal:

```asm
iid  IID_ID3D11Device   ; emits the 16-byte GUID from the database
```

## .ASCIISTRING / .ENDASCIISTRING

Embed raw ASCII text in the data section (used for HLSL shader source):

```asm
shaderSrc:
.ASCIISTRING
cbuffer cb : register(b0) {
    float4x4 worldViewProj;
};
float4 VSMain(float3 pos : POSITION) : SV_POSITION {
    return mul(worldViewProj, float4(pos, 1));
}
.ENDASCIISTRING
shaderSrcEnd:
```

Length = `shaderSrcEnd - shaderSrc`.

## .include

Include another source file at the point of the directive:

```asm
.include "library/canvas.was"
.include "library/blit.was"
```

Paths are resolved relative to the file containing the `.include`.
