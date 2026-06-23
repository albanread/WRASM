//! studio — the windowed assembler IDE.
//!
//! A two-pane Direct2D window over the same language thread `studio-repl` drives:
//!
//!   * **left** — an editor on the headless [`Doc`](studio::doc::Doc) model:
//!     winkb-aware syntax colours ([`studio::syntax`]), a caret, and red
//!     squiggles from `was::check` ([`studio::diagnostics`]).
//!   * **right** — the assistant: a `docpane`-rendered markdown card for the
//!     symbol under the caret (function / constant / struct), navigated the same
//!     `was:` way the headless seam already models.
//!   * **status** — the live machine-code bytes of the caret's line (the
//!     "see your code" view), the diagnostic count, and **F5** to assemble the
//!     whole buffer to a self-contained `.exe`.
//!
//! All knowledge/assembler work goes through [`studio::lang::Lang`] — the GUI
//! thread never touches winkb or rasm directly. For this first interactive cut
//! the calls are synchronous (each is sub-millisecond on a small buffer); moving
//! them to the async `post_*`/`poll` path is the natural next step, and the
//! worker's request coalescing is already in place for when it does.

#[cfg(not(windows))]
fn main() {
    eprintln!("RASM Studio is Windows-only (it renders through Direct2D).");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    gui::run()
}

#[cfg(windows)]
mod gui {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
    };
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA,
        IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::Graphics::Direct2D::{
        ID2D1HwndRenderTarget, ID2D1RenderTarget, D2D1_FEATURE_LEVEL_DEFAULT,
        D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
        D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
        D2D1_RENDER_TARGET_USAGE_NONE,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{
        GetDpiForWindow, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F12, VK_F5, VK_F6,
        VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_UP, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
        GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassExW, SetCursor, SetTimer,
        SetWindowLongPtrW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA,
        HTCLIENT, IDC_ARROW, IDC_SIZENS, IDC_SIZEWE, MSG, SW_SHOW, WINDOW_EX_STYLE, WM_CHAR,
        WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_TIMER,
        WNDCLASSEXW, WNDCLASS_STYLES, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateMenu, CreatePopupMenu, DeleteMenu, DrawMenuBar, GetMenuItemCount,
        SetMenu, SetWindowTextW, HMENU, MF_BYPOSITION, MF_POPUP, MF_SEPARATOR, MF_STRING, WM_COMMAND,
    };
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    use docpane::layout::Layout;
    use docpane::{layout as dlayout, parser, render, theme};
    use rust_tcl::{Arity, Error as TclError, Registry, Value};

    use studio::diagnostics;
    use studio::doc::Doc;
    use studio::outline::{Label, LabelKind};
    use studio::lang::{Diag, Emit, Lang, ListingRow, Response};
    use studio::syntax::TokKind;
    use winkb::Completion;

    const CLASS: PCWSTR = w!("RasmStudioMain");

    // Editor metrics, in DIPs. A monospace family keeps caret/squiggle maths
    // simple and the editor crisp; DirectWrite falls back if it isn't installed.
    const EDITOR_FONT: &str = theme::CODE_FONT;
    const EDITOR_SIZE: f32 = 15.0;
    const LINE_H: f32 = EDITOR_SIZE * 1.5;
    const TOP_PAD: f32 = 10.0;
    const SPLIT_FRAC: f32 = 0.56;
    const WHEEL_STEP: f32 = 48.0;

    // The ASM-listing left margin: a line number, then the machine-code bytes for
    // each instruction in gray. A macro line (e.g. `invoke`) is followed by gray,
    // view-only "ghost" rows — its literal lowered expansion, `bytes : asm` — so
    // you see the generated code without it touching the source.
    const LN_W: f32 = 34.0; // line-number column
    const BYTES_W: f32 = 188.0; // byte column
    const BYTE_SIZE: f32 = 12.5;
    const SRC_X: f32 = LN_W + BYTES_W + 8.0; // where source / ghost asm begins

    const SEARCH_H: f32 = 40.0; // assistant search-box band
    const OUTPUT_H: f32 = 120.0; // bottom-left output pane — the default/restore height

    // Draggable splitters between the three panes (editor | assistant, editor / output).
    const SPLIT_W: f32 = 8.0; // splitter grab + glow band width (DIPs)
    const SPLIT_HALF: f32 = SPLIT_W * 0.5;
    const TOGGLE_LEN: f32 = 36.0; // collapse button length along the splitter
    const TOGGLE_THICK: f32 = 16.0; // collapse button thickness across the splitter
    const MIN_PANE: f32 = 90.0; // smallest a pane may be dragged to
    const SPLIT_GLOW: u32 = 0x4F_A6_F1; // hover/drag accent (the Info blue)
    const SPLIT_GLOW_BG: u32 = 0x21_33_45; // dim wash behind the glow line
    const TOGGLE_BG: u32 = 0x33_33_3D; // resting collapse-button background

    const SQUIGGLE: u32 = 0xF1_4C_4C; // VS Code error red (Error severity)
    const DIAG_WARN: u32 = 0xE5_C0_7B; // amber/yellow (Warn severity)
    const DIAG_INFO: u32 = 0x4F_A6_F1; // blue (Info severity)

    /// The colour for a diagnostic of `sev`: Error → red, Warn → amber, Info →
    /// blue. The editor squiggle and the output-pane line both use this so the
    /// two views agree on what's an error vs. an advisory note.
    fn diag_color(sev: studio::lang::Severity) -> u32 {
        use studio::lang::Severity;
        match sev {
            Severity::Error => SQUIGGLE,
            Severity::Warn => DIAG_WARN,
            Severity::Info => DIAG_INFO,
        }
    }
    const CARET_COLOR: u32 = theme::TEXT_BRIGHT;
    const BYTE_COLOR: u32 = 0x6E_6E_6E; // listing bytes, dim gray
    const GHOST_COLOR: u32 = 0x86_86_86; // expanded ghost asm, gray
    const SELECTION: u32 = 0x26_4F_78; // selection highlight (VS Code blue)
    const VK_F: VIRTUAL_KEY = VIRTUAL_KEY(0x46); // 'F' (Ctrl+F = search)
    const VK_A: VIRTUAL_KEY = VIRTUAL_KEY(0x41);
    const VK_C: VIRTUAL_KEY = VIRTUAL_KEY(0x43);
    const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);
    const VK_X: VIRTUAL_KEY = VIRTUAL_KEY(0x58);
    const VK_Y: VIRTUAL_KEY = VIRTUAL_KEY(0x59);
    const VK_Z: VIRTUAL_KEY = VIRTUAL_KEY(0x5A);
    const VK_N: VIRTUAL_KEY = VIRTUAL_KEY(0x4E);
    const VK_O: VIRTUAL_KEY = VIRTUAL_KEY(0x4F);
    const VK_S: VIRTUAL_KEY = VIRTUAL_KEY(0x53);
    const VK_G: VIRTUAL_KEY = VIRTUAL_KEY(0x47); // 'G' (Ctrl+G = go to label)

    // The go-to-label palette.
    const FIND_ITEM_H: f32 = 22.0;
    const FIND_HEADER_H: f32 = 30.0;
    const FIND_MAX_VIS: usize = 12;
    const CODE_LABEL_COL: u32 = 0x61_AF_EF; // code labels — blue
    const DATA_LABEL_COL: u32 = 0x98_C3_79; // data labels — green
    const PALETTE_BG: u32 = 0x22_22_2B;
    const PALETTE_HEADER_BG: u32 = 0x2C_2C_38;
    const PALETTE_SEL: u32 = 0x2D_3D_55;

    // Menu command ids (WM_COMMAND low word).
    const IDM_NEW: u16 = 0x100;
    const IDM_OPEN: u16 = 0x101;
    const IDM_SAVE: u16 = 0x102;
    const IDM_SAVEAS: u16 = 0x103;
    const IDM_EXPORT: u16 = 0x104;
    const IDM_EXIT: u16 = 0x105;
    const IDM_UNDO: u16 = 0x110;
    const IDM_REDO: u16 = 0x111;
    const IDM_CUT: u16 = 0x112;
    const IDM_COPY: u16 = 0x113;
    const IDM_PASTE: u16 = 0x114;
    const IDM_SELALL: u16 = 0x115;
    const IDM_BUILD: u16 = 0x120;
    const IDM_RUN: u16 = 0x121;
    const IDM_RECENT_BASE: u16 = 0x200; // recent files occupy base .. base + N

    const STARTER: &str = "\
.globl main

APPEND MACRO chr            ; a user macro (MASM-style) — no code until used
  mov al, chr
  mov [rdi + rcx], al
  inc rcx
ENDM

banner BYTE \"WRASM: \", 0   ; global data — a string, shown with its bytes

main:
  sub rsp, 64               ; a buffer + scratch on the stack
  lea rdi, [rsp + 32]       ; rdi = &buffer
  xor rcx, rcx              ; rcx = index

  .for al = '0' to '9'      ; a counted loop: digits 0..9
    mov [rdi + rcx], al
    inc rcx
  .endfor

  APPEND '!'                ; macro invocations expand inline below
  APPEND '!'

  mov r13, rcx              ; the byte count (saved before invoke clobbers rcx)
  invoke GetStdHandle, -11  ; STD_OUTPUT_HANDLE -> rax
  mov rsi, rax              ; rsi = stdout
  lea r12, [rsp + 24]       ; r12 = &bytesWritten
  invoke WriteFile, rsi, banner, 7, r12, 0  ; the global string
  invoke WriteFile, rsi, rdi, r13, r12, 0   ; the built buffer
  invoke ExitProcess, 0
";

    /// A token's editor colour (VS Code Dark+ family).
    fn tok_color(k: TokKind) -> u32 {
        match k {
            TokKind::Comment => 0x6A_99_55,
            TokKind::Label => 0xDC_DC_AA,
            TokKind::Directive => 0xC5_86_C0,
            TokKind::Mnemonic => 0x56_9C_D6,
            TokKind::Keyword => 0xC5_86_C0,
            TokKind::Register => 0x9C_DC_FE,
            TokKind::Number => 0xB5_CE_A8,
            TokKind::String => 0xCE_91_78,
            TokKind::Constant => 0x4E_C9_B0,
            TokKind::Ident => 0xD4_D4_D4,
            TokKind::Punct => 0x80_80_80,
        }
    }

    /// Accurate width (DIPs) of an editor text run, via DirectWrite.
    fn measure(s: &str) -> f32 {
        if s.is_empty() {
            0.0
        } else {
            render::measure_text(s, EDITOR_FONT, EDITOR_SIZE, false, false)
        }
    }

    /// How many visual rows `text` wraps to in `max_w` at `size` — a greedy
    /// word-wrap over measured widths (matches the renderer), so a long ghost
    /// line (a wide string's many `.word` values, or its many bytes) reserves
    /// the room it actually needs instead of overprinting the next line.
    fn wrap_rows(text: &str, max_w: f32, size: f32) -> f32 {
        if text.trim().is_empty() {
            return 1.0;
        }
        let max_w = max_w.max(1.0);
        let m = |s: &str| render::measure_text(s, EDITOR_FONT, size, false, false);
        let mut rows = 1.0_f32;
        let mut x = 0.0_f32;
        let mut rest = text;
        while !rest.is_empty() {
            let ws_end = rest.find(|c: char| !c.is_whitespace()).unwrap_or(rest.len());
            let ws_w = if ws_end == 0 { 0.0 } else { m(&rest[..ws_end]) };
            rest = &rest[ws_end..];
            if rest.is_empty() {
                break;
            }
            let w_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let word_w = m(&rest[..w_end]);
            rest = &rest[w_end..];
            if x > 0.0 && x + ws_w + word_w > max_w {
                rows += 1.0;
                x = word_w;
            } else {
                x += ws_w + word_w;
            }
            while x > max_w {
                rows += 1.0;
                x -= max_w;
            }
        }
        rows
    }

    /// Hex for a listing row, rendering reloc-placeholder bytes (extern fields,
    /// resolved at link) as `??` instead of a misleading `00`.
    fn hex_masked(bytes: &[u8], mask: &[bool]) -> String {
        bytes
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if mask.get(i).copied().unwrap_or(false) {
                    "??".to_string()
                } else {
                    format!("{b:02x}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Put `text` on the system clipboard as UTF-16 (`CF_UNICODETEXT`).
    unsafe fn clipboard_set(hwnd: HWND, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(13, Some(HANDLE(h.0))); // 13 = CF_UNICODETEXT
            }
        }
        let _ = CloseClipboard();
    }

    /// Read UTF-16 text from the system clipboard, if present.
    unsafe fn clipboard_get(hwnd: HWND) -> Option<String> {
        if OpenClipboard(Some(hwnd)).is_err() {
            return None;
        }
        let mut out = None;
        if IsClipboardFormatAvailable(13).is_ok() {
            if let Ok(h) = GetClipboardData(13) {
                let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
                if !p.is_null() {
                    let mut len = 0;
                    while *p.add(len) != 0 {
                        len += 1;
                    }
                    out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                    let _ = GlobalUnlock(HGLOBAL(h.0));
                }
            }
        }
        let _ = CloseClipboard();
        out
    }

    // ── menu + dialog + recent-file helpers ──────────────────────────────────

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    unsafe fn menu_str(m: HMENU, id: u16, label: &str) {
        let w = wide(label);
        let _ = AppendMenuW(m, MF_STRING, id as usize, PCWSTR(w.as_ptr()));
    }
    unsafe fn menu_popup(m: HMENU, sub: HMENU, label: &str) {
        let w = wide(label);
        let _ = AppendMenuW(m, MF_POPUP, sub.0 as usize, PCWSTR(w.as_ptr()));
    }
    unsafe fn menu_sep(m: HMENU) {
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
    }

    /// A common file dialog; `save` picks open vs. save, `ext` the default
    /// extension for saving. Returns the chosen path.
    unsafe fn file_dialog(hwnd: HWND, save: bool, ext: &str) -> Option<PathBuf> {
        let mut buf = [0u16; 1024];
        let filter: Vec<u16> = "WRASM source\0*.was\0Listing\0*.lst\0All files\0*.*\0\0"
            .encode_utf16()
            .collect();
        let defext = wide(ext);
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: hwnd,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(buf.as_mut_ptr()),
            nMaxFile: buf.len() as u32,
            lpstrDefExt: PCWSTR(defext.as_ptr()),
            Flags: if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST
            },
            ..Default::default()
        };
        let ok = if save {
            GetSaveFileNameW(&mut ofn).as_bool()
        } else {
            GetOpenFileNameW(&mut ofn).as_bool()
        };
        if !ok {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..end])))
    }

    fn recent_file() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("rasm-studio").join("recent.txt"))
    }
    fn load_recent() -> Vec<PathBuf> {
        recent_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
            .unwrap_or_default()
    }
    fn save_recent(recent: &[PathBuf]) {
        if let Some(p) = recent_file() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let body =
                recent.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
            let _ = std::fs::write(p, body);
        }
    }

    /// The two draggable splitters: `Col` is the vertical editor | assistant
    /// divider; `Row` is the horizontal editor / output divider (left column).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Split {
        Col,
        Row,
    }

    struct App {
        hwnd: HWND,
        dpi: u32,
        client_w: u32,
        client_h: u32,
        target: Option<ID2D1HwndRenderTarget>,

        lang: Option<Lang>,
        doc: Doc,
        diags: Vec<Diag>,
        /// The lowered listing per source line — a [`ListingRow`] for each
        /// expanded instruction (re-fetched on every edit).
        line_listing: Vec<Vec<ListingRow>>,
        /// A short status line for the output pane (build result, snapshot path…).
        notice: String,
        /// Vertical scroll of the editor pane, in DIPs (0 = top).
        editor_scroll: f32,
        /// Last pointer position in DIPs (client), to route the wheel by pane.
        last_mouse: (f32, f32),
        /// Whether the initial caret has been scrolled into view (one-shot).
        revealed: bool,

        // Assistant: a search box + a card.
        search: String,
        search_active: bool,
        card_md: String,
        card_word: String,
        card_layout: Option<Layout>,
        card_laid_w: f32,
        card_scroll: f32,
        card_max_scroll: f32,

        /// Ids of the latest async per-keystroke requests; a reply is applied only
        /// if it still matches (superseded replies are dropped).
        pending_check: u64,
        pending_listing: u64,
        pending_card: u64,
        /// True while the mouse is dragging out a selection.
        dragging: bool,

        /// The file the buffer is associated with (`None` = untitled).
        path: Option<PathBuf>,
        /// Recently opened files (most recent first).
        recent: Vec<PathBuf>,
        /// The "Open Recent" popup, rebuilt as `recent` changes.
        recent_menu: HMENU,
        /// `comobj NAME : Interface` bindings in the buffer (for COM-aware cards).
        com_binds: Vec<(String, String)>,
        /// The buffer's user-defined equates, cached from the language thread
        /// (the editor has no `Kb`): drives equate hover + go-to-definition.
        equates: Vec<was::EquateDef>,
        /// The in-flight equates-refresh request id (matches an `Equates` reply).
        pending_equates: u64,
        /// Active autocomplete candidates (empty = the popup is hidden).
        comp: Vec<Completion>,
        /// Highlighted candidate index.
        comp_sel: usize,
        /// Byte offset in the caret line where the completed prefix starts.
        comp_start: usize,
        /// The in-flight completion request id (matches a `Completions` reply).
        pending_complete: u64,

        /// Vertical split position as a fraction of the viewport width.
        split_frac: f32,
        /// Output-pane height in DIPs (restored when reopened).
        output_h: f32,
        /// Pane visibility — a collapsed pane shows only its splitter + toggle.
        assistant_open: bool,
        output_open: bool,
        /// The splitter the pointer is over (drives the glow + resize cursor).
        hover_split: Option<Split>,
        /// The splitter being dragged, if any.
        drag_split: Option<Split>,

        /// Go-to-label palette: open state, the filter query, the highlighted
        /// match, and the buffer's labels (gathered when the palette opens).
        find_active: bool,
        find_query: String,
        find_sel: usize,
        find_labels: Vec<Label>,
    }

    impl App {
        fn new(hwnd: HWND, lang: Option<Lang>) -> Self {
            let dpi = match unsafe { GetDpiForWindow(hwnd) } {
                0 => 96,
                d => d,
            };
            let notice = if lang.is_none() {
                "knowledge db not found — set WINKB_DB; editing + highlighting only".to_string()
            } else {
                String::new()
            };
            let mut app = App {
                hwnd,
                dpi,
                client_w: 0,
                client_h: 0,
                target: None,
                lang,
                doc: Doc::from_str(STARTER.trim_end()),
                diags: Vec::new(),
                line_listing: Vec::new(),
                notice,
                search: String::new(),
                search_active: false,
                card_md: welcome_card(),
                card_word: String::new(),
                card_layout: None,
                card_laid_w: -1.0,
                card_scroll: 0.0,
                card_max_scroll: 0.0,
                editor_scroll: 0.0,
                last_mouse: (0.0, 0.0),
                revealed: false,
                pending_check: 0,
                pending_listing: 0,
                pending_card: 0,
                dragging: false,
                path: None,
                recent: load_recent(),
                recent_menu: HMENU::default(),
                com_binds: Vec::new(),
                equates: Vec::new(),
                pending_equates: 0,
                comp: Vec::new(),
                comp_sel: 0,
                comp_start: 0,
                pending_complete: 0,
                split_frac: SPLIT_FRAC,
                output_h: OUTPUT_H,
                assistant_open: true,
                output_open: true,
                hover_split: None,
                drag_split: None,
                find_active: false,
                find_query: String::new(),
                find_sel: 0,
                find_labels: Vec::new(),
            };
            unsafe { app.build_menu() };
            app.update_title();
            // Open on the macro definition — it generates no code (empty margin),
            // with the loop and the macro's expansion in the listing below.
            app.doc.set_caret(2, 2);
            app.seed();
            app
        }

        fn dip_scale(&self) -> f32 {
            96.0 / self.dpi as f32
        }
        /// Viewport size in DIPs (the target is DPI-aware, so we draw in DIPs).
        fn viewport(&self) -> (f32, f32) {
            let s = self.dip_scale();
            (self.client_w as f32 * s, self.client_h as f32 * s)
        }
        fn invalidate(&self) {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }

        // ── splitter geometry (single source of truth for pane bounds) ───────

        /// X of the vertical splitter (editor | assistant). Collapsed → the
        /// splitter parks at the right edge so only its toggle shows.
        fn split_x(&self, vw: f32) -> f32 {
            if !self.assistant_open {
                return (vw - SPLIT_W).max(0.0);
            }
            let hi = (vw - MIN_PANE).max(MIN_PANE);
            (vw * self.split_frac).clamp(MIN_PANE, hi)
        }

        /// Y of the horizontal splitter (editor / output). Collapsed → it parks
        /// at the bottom so only its toggle shows.
        fn split_y(&self, vh: f32) -> f32 {
            if !self.output_open {
                return (vh - SPLIT_W).max(0.0);
            }
            let hi = (vh - MIN_PANE).max(MIN_PANE);
            (vh - self.output_h).clamp(MIN_PANE, hi)
        }

        /// The splitter whose grab band contains the point (a touch wider than the
        /// drawn line for easy grabbing), or `None`.
        fn splitter_at(&self, x: f32, y: f32) -> Option<Split> {
            let (vw, vh) = self.viewport();
            let sx = self.split_x(vw);
            if (x - sx).abs() <= SPLIT_HALF + 1.5 {
                return Some(Split::Col);
            }
            let sy = self.split_y(vh);
            if x < sx && (y - sy).abs() <= SPLIT_HALF + 1.5 {
                return Some(Split::Row);
            }
            None
        }

        /// The collapse-toggle rect `(x, y, w, h)` for a splitter — `< >` midway
        /// down the vertical one, `^ v` midway across the horizontal one.
        fn toggle_rect(&self, s: Split, vw: f32, vh: f32) -> (f32, f32, f32, f32) {
            let sx = self.split_x(vw);
            match s {
                Split::Col => (sx - TOGGLE_THICK * 0.5, vh * 0.5 - TOGGLE_LEN * 0.5, TOGGLE_THICK, TOGGLE_LEN),
                Split::Row => {
                    let sy = self.split_y(vh);
                    (sx * 0.5 - TOGGLE_LEN * 0.5, sy - TOGGLE_THICK * 0.5, TOGGLE_LEN, TOGGLE_THICK)
                }
            }
        }

        /// The splitter whose toggle button contains the point, or `None`.
        fn toggle_at(&self, x: f32, y: f32) -> Option<Split> {
            let (vw, vh) = self.viewport();
            [Split::Col, Split::Row].into_iter().find(|&s| {
                let (rx, ry, rw, rh) = self.toggle_rect(s, vw, vh);
                x >= rx && x < rx + rw && y >= ry && y < ry + rh
            })
        }

        // ── language-thread refresh (synchronous; sub-ms on a small buffer) ──

        /// After a text edit: (re)request a check and the per-line listing
        /// asynchronously, then update the caret-driven card. Replies arrive on
        /// the poll timer; superseded ones are dropped by id.
        fn after_edit(&mut self) {
            if let Some(lang) = self.lang.as_ref() {
                let text = self.doc.text();
                self.pending_check = lang.post_check(&text);
                self.pending_listing = lang.post_listing(&text);
                // The equate table needs a `Kb` to fold values, so it's computed
                // on the worker; cache the reply for hover + go-to-definition.
                self.pending_equates = lang.post_equates(&text);
            }
            self.com_binds = was::com_bindings(&self.doc.text());
            self.after_caret();
        }

        /// The card query for the caret: a `comobj`'s interface, an `obj.Method`
        /// (→ `Interface::Method`), or else the plain symbol under the caret.
        fn card_query(&self) -> Option<String> {
            let line = self.doc.line(self.doc.caret.row);
            // The dotted token around the caret (`pSwap` or `pSwap.Present`).
            let is_tok = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
            let col = self.doc.caret.col.min(line.len());
            let mut s = col;
            while s > 0 {
                let c = line[..s].chars().next_back().unwrap();
                if is_tok(c) {
                    s -= c.len_utf8();
                } else {
                    break;
                }
            }
            let mut e = col;
            while e < line.len() {
                let c = line[e..].chars().next().unwrap();
                if is_tok(c) {
                    e += c.len_utf8();
                } else {
                    break;
                }
            }
            let tok = &line[s..e];
            if let Some((name, method)) = tok.split_once('.') {
                if let Some((_, iface)) = self.com_binds.iter().find(|(n, _)| n == name) {
                    return Some(format!("{iface}::{method}"));
                }
                // Not a comobj — a `Struct.field` access (e.g. WNDCLASSEXW.cbSize).
                // Hand the resolver the dotted token; it shows the struct's card.
                if !name.is_empty() {
                    return Some(tok.to_string());
                }
            } else if let Some((_, iface)) = self.com_binds.iter().find(|(n, _)| n == tok) {
                return Some(iface.clone());
            }
            lang_word(&self.doc)
        }

        /// After a caret move (no text change): request the card for the symbol
        /// under the caret (async) and keep the caret on screen.
        fn after_caret(&mut self) {
            if !self.search_active {
                if let Some(word) = self.card_query() {
                    if word != self.card_word {
                        // Your own symbols (local labels/data/struct instances)
                        // resolve straight from the buffer — winkb has no card for
                        // them. Everything else is an async db lookup.
                        if let Some(md) = ide::local_card(&self.doc.text(), &word) {
                            self.card_md = md;
                            self.card_layout = None;
                            self.card_scroll = 0.0;
                        } else if let Some(lang) = self.lang.as_ref() {
                            self.pending_card = lang.post_card(&word);
                        }
                        self.card_word = word; // tentative; the reply fills card_md
                    }
                }
            }
            self.ensure_caret_visible();
        }

        /// Ask for autocomplete candidates at the caret (member completion after
        /// `.`, or a name once a prefix is typed); hide the popup otherwise.
        fn maybe_complete(&mut self) {
            use studio::complete::CompletionKind as K;
            let line = self.doc.line(self.doc.caret.row).to_string();
            let col = self.doc.caret.col;
            let ctx = studio::complete::context(&line, col);
            let want = match &ctx.kind {
                K::Field { .. } => true,
                K::Function | K::Symbol => !ctx.prefix.is_empty(),
                K::None => false,
            };
            if want {
                if let Some(lang) = self.lang.as_ref() {
                    self.pending_complete = lang.post_complete(&line, col, self.com_binds.clone());
                }
            } else {
                self.close_completion();
            }
        }

        fn close_completion(&mut self) {
            if !self.comp.is_empty() {
                self.comp.clear();
                self.invalidate();
            }
        }

        /// Replace the typed prefix with the highlighted candidate (one undo step).
        fn accept_completion(&mut self) {
            if self.comp.is_empty() {
                return;
            }
            let name = self.comp[self.comp_sel].name.clone();
            let row = self.doc.caret.row;
            let col = self.doc.caret.col;
            self.doc.set_caret(row, self.comp_start.min(col));
            self.doc.start_selection();
            self.doc.set_caret(row, col);
            self.doc.insert(&name);
            self.comp.clear();
            self.after_edit();
            self.invalidate();
        }

        /// Synchronous one-time population for the first paint (before the poll
        /// timer runs); all runtime updates go through the async path above.
        fn seed(&mut self) {
            if let Some(lang) = self.lang.as_ref() {
                let text = self.doc.text();
                if let Some(Response::Check { diags, .. }) = lang.check_src(&text) {
                    self.diags = diags;
                }
                if let Some(Response::Listing { rows, .. }) = lang.listing(&text) {
                    self.line_listing = rows;
                }
                if let Some(Response::Equates { equates, .. }) = lang.equates(&text) {
                    self.equates = equates;
                }
            }
            self.com_binds = was::com_bindings(&self.doc.text());
            if !self.search_active {
                self.refresh_card(self.card_query());
            }
            self.ensure_caret_visible();
        }

        /// Drain the language thread's replies, applying only those that still
        /// match the latest request id (the rest are superseded). Runs each
        /// poll-timer tick — the GUI thread never blocks on the worker.
        fn poll_lang(&mut self) {
            while let Some(resp) = self.lang.as_ref().and_then(Lang::poll) {
                match resp {
                    Response::Check { id, diags } if id == self.pending_check => {
                        self.diags = diags;
                        self.invalidate();
                    }
                    Response::Listing { id, rows } if id == self.pending_listing => {
                        self.line_listing = rows;
                        self.invalidate();
                    }
                    Response::Card { id, markdown } if id == self.pending_card => {
                        self.card_md = markdown;
                        self.card_layout = None;
                        self.card_scroll = 0.0;
                        self.invalidate();
                    }
                    Response::Completions { id, items, replace_start }
                        if id == self.pending_complete =>
                    {
                        self.comp = items;
                        self.comp_sel = 0;
                        self.comp_start = replace_start;
                        self.invalidate();
                    }
                    Response::Equates { id, equates } if id == self.pending_equates => {
                        self.equates = equates;
                        self.invalidate();
                    }
                    _ => {} // a superseded reply, or one for a sync call() already taken
                }
            }
        }

        /// The lowered instruction rows for source `row` (empty for a blank line).
        fn listing(&self, row: usize) -> &[ListingRow] {
            self.line_listing.get(row).map(Vec::as_slice).unwrap_or(&[])
        }

        /// A source line is a "macro" worth expanding when it lowers to more than
        /// one instruction (e.g. `invoke` → the whole Win64 call sequence).
        fn is_macro(&self, row: usize) -> bool {
            self.listing(row).len() > 1
        }

        /// Update the assistant card if the caret moved onto a different symbol.
        fn refresh_card(&mut self, word: Option<String>) {
            let Some(word) = word else { return };
            if word == self.card_word {
                return;
            }
            // Equate-first: a name the buffer defines as an equate is this buffer's
            // meaning, so surface its definition (value + source line) ahead of any
            // winkb card. Ctrl+click the name to jump to the definition.
            if let Some(eq) = self.equates.iter().find(|e| e.name == word) {
                self.card_word = word;
                self.card_md = format!(
                    "## {} — your equate\n\n`{} = {}` = **{}**\n\nDefined at line **{}**. \
                     Ctrl+click the name to jump there.\n",
                    eq.name, eq.name, eq.expr, eq.value, eq.line,
                );
                self.card_layout = None;
                self.card_scroll = 0.0;
                return;
            }
            if let Some(md) = ide::local_card(&self.doc.text(), &word) {
                self.card_word = word;
                self.card_md = md;
                self.card_layout = None;
                self.card_scroll = 0.0;
                return;
            }
            if let Some(lang) = self.lang.as_ref() {
                if let Some(Response::Card { markdown, .. }) = lang.card(&word) {
                    self.card_word = word;
                    self.card_md = markdown;
                    self.card_layout = None; // force relayout
                    self.card_scroll = 0.0;
                }
            }
        }

        /// Run the assistant search box: a free-form winkb query (the same
        /// `ide::answer` the caret uses — exact match → card, else search results
        /// with clickable `was:` links).
        fn run_search(&mut self) {
            let q = self.search.trim().to_string();
            if q.is_empty() {
                return;
            }
            if let Some(lang) = self.lang.as_ref() {
                if let Some(Response::Card { markdown, .. }) = lang.card(&q) {
                    self.card_word = q; // suppress the caret clobbering it
                    self.card_md = markdown;
                    self.card_layout = None;
                    self.card_scroll = 0.0;
                }
            }
            self.invalidate();
        }

        /// Where the built exe goes: next to the saved source (so its name is
        /// obvious and any files it writes land beside it), else a temp file.
        fn output_exe_path(&self) -> PathBuf {
            match &self.path {
                Some(p) => p.with_extension("exe"),
                None => std::env::temp_dir().join("studio_build.exe"),
            }
        }

        /// The buffer with `.include "file"`s expanded (relative to the saved file's
        /// directory), so a multi-file program builds/runs in the IDE exactly as it
        /// does from the `was` CLI. An unsaved buffer has no base directory, so its
        /// includes can't resolve — save first. A missing include is surfaced.
        fn build_source(&self) -> Result<String, String> {
            let text = self.doc.text();
            match &self.path {
                Some(p) => was::expand_includes(&text, p).map_err(|e| e.to_string()),
                None => Ok(text),
            }
        }

        /// Assemble the whole buffer to a self-contained exe and report.
        fn build_exe(&mut self) {
            let Some(lang) = self.lang.as_ref() else { return };
            let out = self.output_exe_path();
            let src = match self.build_source() {
                Ok(s) => s,
                Err(e) => {
                    self.notice = format!("include error: {e}");
                    self.invalidate();
                    return;
                }
            };
            self.notice = match lang.assemble(&src, Emit::Exe) {
                Some(Response::Assembled { bytes, info, .. }) => match std::fs::write(&out, &bytes) {
                    Ok(()) => format!("built {} — {info}", out.display()),
                    Err(e) => format!("build ok ({info}) but write failed: {e}"),
                },
                Some(Response::Error { message, .. }) => format!("build error: {message}"),
                _ => "build failed: no reply from the assembler (timed out)".to_string(),
            };
            self.invalidate();
        }

        // ── rendering ────────────────────────────────────────────────────────

        fn ensure_target(&mut self, w: u32, h: u32) {
            if let Some(t) = self.target.as_ref() {
                let cur = unsafe { t.GetPixelSize() };
                if cur.width != w || cur.height != h {
                    let _ = unsafe { t.Resize(&D2D_SIZE_U { width: w, height: h }) };
                }
                return;
            }
            let dpi = self.dpi as f32;
            let made = unsafe {
                render::factory().CreateHwndRenderTarget(
                    &D2D1_RENDER_TARGET_PROPERTIES {
                        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_IGNORE,
                        },
                        dpiX: dpi,
                        dpiY: dpi,
                        usage: D2D1_RENDER_TARGET_USAGE_NONE,
                        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                    },
                    &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                        hwnd: self.hwnd,
                        pixelSize: D2D_SIZE_U { width: w, height: h },
                        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                    },
                )
            };
            match made {
                Ok(t) => self.target = Some(t),
                Err(e) => eprintln!("[studio] CreateHwndRenderTarget failed: {e}"),
            }
        }

        /// (Re)lay out the assistant card for the current pane width. The card
        /// begins at `y0` (below the search band) so its baked coordinates line up
        /// with where `draw_document` puts it.
        fn relayout_card(&mut self, x_base: f32, width: f32, y0: f32) {
            let stale = self.card_layout.is_none() || (self.card_laid_w - width).abs() > 0.5;
            if stale {
                let md = if self.card_md.trim().is_empty() {
                    welcome_card()
                } else {
                    self.card_md.clone()
                };
                let blocks = parser::parse(&md);
                self.card_layout =
                    Some(dlayout::layout(&blocks, x_base, width, y0, render::measure_text));
                self.card_laid_w = width;
            }
        }

        fn paint(&mut self) {
            let mut rect = RECT::default();
            if unsafe { GetClientRect(self.hwnd, &mut rect) }.is_err() {
                return;
            }
            let (w, h) = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);
            if w == 0 || h == 0 {
                return;
            }
            self.client_w = w;
            self.client_h = h;
            self.ensure_target(w, h);
            let target = match self.target.clone() {
                Some(t) => t,
                None => return,
            };
            // Now that the viewport size is known, scroll the initial caret in.
            if !self.revealed {
                self.revealed = true;
                self.ensure_caret_visible();
            }
            let (vw, vh) = self.viewport();
            let base: &ID2D1RenderTarget = &target;
            unsafe { self.render_frame(base, vw, vh) };
        }

        /// Draw a whole frame — editor + assistant + status — into any target at
        /// a viewport size in DIPs. Shared by the live window and the offscreen
        /// [`snapshot`](App::snapshot); owns `BeginDraw`/`EndDraw`.
        unsafe fn render_frame(&mut self, target: &ID2D1RenderTarget, vw: f32, vh: f32) {
            let split = self.split_x(vw);
            let editor_h = self.split_y(vh);

            // Assistant card: laid out below the search band, right of the split.
            if self.assistant_open {
                let card_x = split + theme::H_PAD;
                let card_w = (vw - card_x - theme::H_PAD).max(80.0);
                self.relayout_card(card_x, card_w, SEARCH_H + 4.0);
                let total = self.card_layout.as_ref().map(|l| l.total_h).unwrap_or(0.0);
                self.card_max_scroll = (total - vh).max(0.0);
                self.card_scroll = self.card_scroll.min(self.card_max_scroll);
            }

            target.BeginDraw();
            let bg = theme::hex(theme::BG);
            target.Clear(Some(std::ptr::addr_of!(bg)));

            self.draw_editor(target, split, editor_h);
            self.draw_completion(target, split, editor_h);
            if self.output_open {
                self.draw_output(target, split, editor_h, vh);
            }

            if self.assistant_open {
                if let Some(c) = self.card_layout.as_ref() {
                    let _ = render::draw_document(target, c, self.card_scroll, vh);
                }
                self.draw_search(target, split, vw);
            }

            // The draggable splitters (glow + collapse toggles), on top of all.
            self.draw_splitters(target, vw, vh);

            // The go-to-label palette is a modal overlay — drawn last.
            if self.find_active {
                self.draw_find(target, split, vh);
            }

            let _ = target.EndDraw(None, None);
        }

        /// Draw the two splitters: a thin divider normally, a glowing band when
        /// hovered or dragged, each with a collapse toggle midway along it.
        unsafe fn draw_splitters(&self, t: &ID2D1RenderTarget, vw: f32, vh: f32) {
            let sx = self.split_x(vw);
            let sy = self.split_y(vh);

            let col_lit =
                self.hover_split == Some(Split::Col) || self.drag_split == Some(Split::Col);
            if col_lit {
                render::fill_rect(t, sx - SPLIT_HALF, 0.0, SPLIT_W, vh, SPLIT_GLOW_BG);
                render::fill_rect(t, sx - 0.9, 0.0, 1.8, vh, SPLIT_GLOW);
            } else {
                render::fill_rect(t, sx - 0.75, 0.0, 1.5, vh, theme::BORDER);
            }

            // The horizontal splitter spans the editor/output column only.
            let row_lit =
                self.hover_split == Some(Split::Row) || self.drag_split == Some(Split::Row);
            if row_lit {
                render::fill_rect(t, 0.0, sy - SPLIT_HALF, sx, SPLIT_W, SPLIT_GLOW_BG);
                render::fill_rect(t, 0.0, sy - 0.9, sx, 1.8, SPLIT_GLOW);
            } else {
                render::fill_rect(t, 0.0, sy - 0.75, sx, 1.5, theme::BORDER);
            }

            self.draw_toggle(t, Split::Col, vw, vh, col_lit);
            self.draw_toggle(t, Split::Row, vw, vh, row_lit);
        }

        /// Draw one collapse toggle: a small button with a chevron whose
        /// direction shows what a click does (collapse when open, reveal when not).
        unsafe fn draw_toggle(&self, t: &ID2D1RenderTarget, s: Split, vw: f32, vh: f32, lit: bool) {
            let (rx, ry, rw, rh) = self.toggle_rect(s, vw, vh);
            render::fill_rect(t, rx, ry, rw, rh, if lit { SPLIT_GLOW } else { TOGGLE_BG });
            let glyph = match s {
                Split::Col => {
                    if self.assistant_open {
                        ">"
                    } else {
                        "<"
                    }
                }
                Split::Row => {
                    if self.output_open {
                        "v"
                    } else {
                        "^"
                    }
                }
            };
            let sz = EDITOR_SIZE * 0.95;
            let gw = render::measure_text(glyph, EDITOR_FONT, sz, true, false);
            let gx = rx + (rw - gw) * 0.5;
            let gy = ry + (rh - sz) * 0.5 - 1.0;
            let fg = if lit { 0x10_14_18 } else { theme::TEXT };
            render::draw_text(t, gx, gy, rw, rh, glyph, EDITOR_FONT, sz, true, false, fg, false);
        }

        /// Render the current state into an offscreen WIC bitmap and write it to a
        /// timestamped PNG in `dir`. This re-runs the exact same `render_frame`
        /// the window uses, so the file is pixel-for-pixel what's on screen — but
        /// it needs no visible desktop, which is what makes it reviewable headless
        /// (and is the whole point of the `--shot` mode). Returns the file path.
        fn snapshot(&mut self, dir: &Path) -> anyhow::Result<PathBuf> {
            let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let path = dir.join(format!("studio_shot_{stamp}.png"));
            self.render_png(&path)?;
            Ok(path)
        }

        /// Render the current frame to `path` as a PNG via an offscreen WIC
        /// target — the same `render_frame` the window draws, no desktop needed.
        /// Shared by `--shot`, the F12 snapshot, and the TCL `screenshot` verb.
        fn render_png(&mut self, path: &Path) -> anyhow::Result<()> {
            // Headless (no window yet): adopt a default viewport and reveal the
            // caret, so the frame matches a real first paint.
            if self.client_w == 0 {
                self.client_w = 1100;
                self.client_h = 720;
                self.ensure_caret_visible();
            }
            let (vw, vh) = self.viewport();
            let (pw, ph) = (vw.ceil() as u32, vh.ceil() as u32);

            unsafe {
                let wic: IWICImagingFactory =
                    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                let bitmap =
                    wic.CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;

                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = render::factory().CreateWicBitmapRenderTarget(&bitmap, &props)?;
                self.render_frame(&rt, vw, vh);

                // Encode the WIC bitmap to a PNG file.
                let stream = wic.CreateStream()?;
                let wpath: Vec<u16> =
                    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                stream.InitializeFromFilename(PCWSTR(wpath.as_ptr()), 0x4000_0000)?; // GENERIC_WRITE
                let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
                encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;

                let mut frame = None;
                let mut bag = None;
                encoder.CreateNewFrame(&mut frame, &mut bag)?;
                let frame = frame.ok_or_else(|| anyhow::anyhow!("WIC gave no frame encoder"))?;
                frame.Initialize(bag.as_ref())?;
                frame.SetSize(pw, ph)?;
                let mut fmt = GUID_WICPixelFormat32bppPBGRA;
                frame.SetPixelFormat(&mut fmt)?;
                frame.WriteSource(&bitmap, std::ptr::null())?;
                frame.Commit()?;
                encoder.Commit()?;
            }
            Ok(())
        }

        /// Per-source-row `(top, height)` in DIPs. A row is one text line tall,
        /// plus one line per expanded instruction when it's a macro — so the
        /// `invoke` source line is followed by its lowered ghost rows.
        fn row_layout(&self) -> Vec<(f32, f32)> {
            let mut out = Vec::with_capacity(self.doc.line_count());
            let mut y = TOP_PAD;
            for row in 0..self.doc.line_count() {
                let extra: f32 = if self.is_macro(row) {
                    self.listing(row).iter().map(|(b, m, a)| self.ghost_line_rows(b, m, a)).sum()
                } else {
                    0.0
                };
                let h = (1.0 + extra) * LINE_H;
                out.push((y, h));
                y += h;
            }
            out
        }

        /// How many text rows a ghost listing line needs — the taller of its asm
        /// column and its byte column, so a wide string's long `.word` line (and
        /// its many bytes) gets the vertical room it wraps into.
        fn ghost_line_rows(&self, bytes: &[u8], mask: &[bool], asm: &str) -> f32 {
            let asm_w = (self.split_x(self.viewport().0) - 6.0 - SRC_X - 14.0).max(20.0);
            let asm_rows = wrap_rows(asm, asm_w, EDITOR_SIZE * 0.92);
            let byte_rows = wrap_rows(&hex_masked(bytes, mask), BYTES_W - 10.0, BYTE_SIZE);
            asm_rows.max(byte_rows)
        }

        /// Draw the editor as an ASM listing: a gray byte margin on the left,
        /// colour-coded source on the right, the macro expansion as gray ghost
        /// rows beneath a macro line, the caret, and diagnostic squiggles.
        /// The autocomplete popup, a list anchored below the caret line.
        unsafe fn draw_completion(&self, t: &ID2D1RenderTarget, split: f32, editor_h: f32) {
            if self.comp.is_empty() {
                return;
            }
            let rows = self.row_layout();
            let Some(&(top, _)) = rows.get(self.doc.caret.row) else { return };
            let sy = top - self.editor_scroll;
            if sy < -LINE_H || sy > editor_h {
                return; // caret line scrolled out of view
            }
            let line = self.doc.line(self.doc.caret.row);
            let pe = self.comp_start.min(line.len());
            let popup_w = 320.0_f32;
            let px = (SRC_X + measure(&line[..pe])).min(split - popup_w - 8.0).max(SRC_X);
            let py = sy + LINE_H;
            let item_h = LINE_H;
            let max_show = 8usize;
            let n = self.comp.len().min(max_show);
            // Window the visible items so the selection stays on screen.
            let first = self
                .comp_sel
                .saturating_sub(max_show - 1)
                .min(self.comp.len().saturating_sub(n));
            render::fill_rect(t, px, py, popup_w, n as f32 * item_h + 4.0, 0x22_22_2A);
            render::fill_rect(t, px, py, popup_w, 1.0, theme::BORDER);
            for (i, item) in self.comp.iter().skip(first).take(n).enumerate() {
                let iy = py + 2.0 + i as f32 * item_h;
                if first + i == self.comp_sel {
                    render::fill_rect(t, px, iy, popup_w, item_h, SELECTION);
                }
                render::draw_text(
                    t, px + 7.0, iy, popup_w - 122.0, item_h, &item.name,
                    EDITOR_FONT, EDITOR_SIZE * 0.92, false, false, theme::TEXT, false,
                );
                render::draw_text(
                    t, px + popup_w - 112.0, iy, 106.0, item_h, &item.detail,
                    EDITOR_FONT, EDITOR_SIZE * 0.78, false, false, theme::TEXT_DIM, false,
                );
            }
        }

        unsafe fn draw_editor(&self, t: &ID2D1RenderTarget, split: f32, editor_h: f32) {
            let text_right = split - 6.0;
            render::fill_rect(t, 0.0, 0.0, SRC_X - 4.0, editor_h, 0x18_18_18); // margin bg
            render::fill_rect(t, SRC_X - 4.0, 0.0, 1.0, editor_h, theme::BORDER); // rule

            let rows = self.row_layout();
            for (row, &(top, h)) in rows.iter().enumerate() {
                let sy = top - self.editor_scroll;
                if sy + h < 0.0 {
                    continue; // entirely above the viewport
                }
                if sy > editor_h {
                    break; // and everything below is too
                }
                let line = self.doc.line(row);
                let macro_row = self.is_macro(row);

                // Line number, top-aligned with the source.
                render::draw_text(
                    t, 2.0, sy, LN_W - 6.0, LINE_H, &format!("{:>3}", row + 1),
                    EDITOR_FONT, EDITOR_SIZE * 0.8, false, false, theme::TEXT_DIM, false,
                );

                // A non-macro line shows its bytes in the margin beside the source.
                if !macro_row {
                    if let Some((bytes, mask, _)) = self.listing(row).first() {
                        if !bytes.is_empty() {
                            render::draw_text(
                                t, LN_W + 6.0, sy, BYTES_W - 10.0, LINE_H,
                                &hex_masked(bytes, mask), EDITOR_FONT, BYTE_SIZE, false, false,
                                BYTE_COLOR, false,
                            );
                        }
                    }
                }

                // Selection highlight (behind the source text).
                if let Some((s, e)) = self.doc.selection() {
                    if row >= s.row && row <= e.row {
                        let x0 = if row == s.row {
                            SRC_X + measure(&line[..s.col.min(line.len())])
                        } else {
                            SRC_X
                        };
                        let x1 = if row == e.row {
                            SRC_X + measure(&line[..e.col.min(line.len())])
                        } else {
                            text_right
                        };
                        render::fill_rect(t, x0, sy, (x1 - x0).max(2.0), LINE_H, SELECTION);
                    }
                }

                // Colour-coded source.
                for tok in self.doc.tokens(row) {
                    let x = SRC_X + measure(&line[..tok.start]);
                    if x > text_right {
                        continue;
                    }
                    render::draw_text(
                        t, x, sy, text_right - x, LINE_H, &line[tok.start..tok.end],
                        EDITOR_FONT, EDITOR_SIZE, false, false, tok_color(tok.kind), false,
                    );
                }

                // Squiggles for diagnostics on this row (1-based lines).
                for d in self.diags.iter().filter(|d| d.line == row + 1) {
                    let (s, e) = diagnostics::underline(line, d.col);
                    let ux = SRC_X + measure(&line[..s]);
                    let uw = (measure(&line[..e]) - measure(&line[..s])).max(3.0);
                    render::fill_rect(t, ux, sy + LINE_H - 2.5, uw, 2.0, diag_color(d.severity));
                }

                // Macro expansion: gray ghost rows of `bytes : asm` beneath. Each
                // line is as tall as it wraps, so wide data shows fully.
                if macro_row {
                    let mut gy = sy + LINE_H;
                    for (bytes, mask, asm) in self.listing(row).iter() {
                        let gh = self.ghost_line_rows(bytes, mask, asm) * LINE_H;
                        if gy + gh >= 0.0 && gy <= editor_h {
                            render::draw_text(
                                t, LN_W + 6.0, gy, BYTES_W - 10.0, gh, &hex_masked(bytes, mask),
                                EDITOR_FONT, BYTE_SIZE, false, false, BYTE_COLOR, false,
                            );
                            render::draw_text(
                                t, SRC_X + 14.0, gy, text_right - SRC_X - 14.0, gh, asm,
                                EDITOR_FONT, EDITOR_SIZE * 0.92, false, true, GHOST_COLOR, false,
                            );
                        }
                        gy += gh;
                    }
                }
            }

            // Caret (only ever on a source line, never a ghost row).
            let cr = self.doc.caret;
            if let Some(&(top, _)) = rows.get(cr.row) {
                let sy = top - self.editor_scroll;
                if sy + LINE_H > 0.0 && sy <= editor_h {
                    let cx = SRC_X + measure(&self.doc.line(cr.row)[..cr.col]);
                    render::fill_rect(t, cx, sy + 1.0, 1.8, LINE_H - 2.0, CARET_COLOR);
                }
            }
        }

        /// Draw the output pane below the editor: totals, diagnostics, and the
        /// last action's notice.
        unsafe fn draw_output(&self, t: &ID2D1RenderTarget, w: f32, top: f32, vh: f32) {
            render::fill_rect(t, 0.0, top, w, vh - top, theme::CODE_BG);
            render::fill_rect(t, 0.0, top, w, 1.0, theme::BORDER);
            let pad = 12.0;
            let bytes: usize = self.line_listing.iter().flatten().map(|(b, _, _)| b.len()).sum();
            let insns = self.line_listing.iter().flatten().filter(|(b, _, _)| !b.is_empty()).count();
            render::draw_text(
                t, pad, top + 8.0, w - 2.0 * pad, 16.0,
                &format!("OUTPUT · {} lines · {insns} insns · {bytes} bytes", self.doc.line_count()),
                theme::BODY_FONT, 11.0, true, false, theme::TEXT_DIM, false,
            );

            let mut y = top + 30.0;
            if self.diags.is_empty() {
                render::draw_text(
                    t, pad, y, w - 2.0 * pad, 16.0, "no diagnostics",
                    theme::BODY_FONT, 12.0, false, false, theme::BLOCKQUOTE, false,
                );
            } else {
                for d in self.diags.iter().take(3) {
                    let s = if d.line == 0 {
                        format!("• {}", d.message)
                    } else {
                        format!("• {}:{}  {}", d.line, d.col, d.message)
                    };
                    render::draw_text(
                        t, pad, y, w - 2.0 * pad, 16.0, &s,
                        theme::BODY_FONT, 12.0, false, false, diag_color(d.severity), false,
                    );
                    y += 18.0;
                    if y > vh - 20.0 {
                        break;
                    }
                }
            }
            if !self.notice.is_empty() {
                render::draw_text(
                    t, pad, vh - 22.0, w - 2.0 * pad, 16.0, &self.notice,
                    theme::BODY_FONT, 11.5, false, false, theme::H1, false,
                );
            }
        }

        /// Draw the assistant search box in the band above the card.
        unsafe fn draw_search(&self, t: &ID2D1RenderTarget, x0: f32, vw: f32) {
            let w = vw - x0;
            render::fill_rect(t, x0, 0.0, w, SEARCH_H, theme::BG); // cover any scrolled card
            let pad = 8.0;
            let (bx, by, bw, bh) = (x0 + pad, 6.0, w - 2.0 * pad, SEARCH_H - 12.0);
            let bg = if self.search_active { theme::SIDEBAR_SEL } else { theme::SIDEBAR_BG };
            let bd = if self.search_active { theme::LINK } else { theme::BORDER };
            render::fill_rect(t, bx, by, bw, bh, bg);
            render::fill_rect(t, bx, by, bw, 1.0, bd);
            render::fill_rect(t, bx, by + bh - 1.0, bw, 1.0, bd);
            render::fill_rect(t, bx, by, 1.0, bh, bd);
            render::fill_rect(t, bx + bw - 1.0, by, 1.0, bh, bd);

            let (tx, ty) = (bx + 8.0, by + (bh - 15.0) * 0.5);
            if self.search.is_empty() && !self.search_active {
                render::draw_text(
                    t, tx, ty, bw - 16.0, 18.0, "Search the Windows API…  (Ctrl+F)",
                    theme::BODY_FONT, 13.0, false, true, theme::TEXT_DIM, false,
                );
            } else {
                render::draw_text(
                    t, tx, ty, bw - 16.0, 18.0, &self.search,
                    theme::BODY_FONT, 13.0, false, false, theme::TEXT, false,
                );
                if self.search_active {
                    let cx = tx + render::measure_text(&self.search, theme::BODY_FONT, 13.0, false, false);
                    render::fill_rect(t, cx + 1.0, ty, 1.5, 16.0, CARET_COLOR);
                }
            }
            render::fill_rect(t, x0, SEARCH_H, w, 1.0, theme::BORDER);
        }

        // ── go-to-label palette ────────────────────────────────────────────

        /// Open the palette: gather the buffer's labels, reset the filter.
        fn find_open(&mut self) {
            self.find_labels = studio::outline::classified(&self.doc.text());
            self.find_query.clear();
            self.find_sel = 0;
            self.find_active = true;
            self.close_completion();
            self.invalidate();
        }

        /// Labels matching the query (case-insensitive substring), prefix
        /// matches first, then source order.
        fn find_matches(&self) -> Vec<&Label> {
            let q = self.find_query.to_ascii_lowercase();
            let mut v: Vec<&Label> = self
                .find_labels
                .iter()
                .filter(|l| q.is_empty() || l.name.to_ascii_lowercase().contains(&q))
                .collect();
            if !q.is_empty() {
                v.sort_by_key(|l| (!l.name.to_ascii_lowercase().starts_with(&q), l.line));
            }
            v
        }

        /// Jump to the highlighted label and close the palette.
        fn find_accept(&mut self) {
            let target = self.find_matches().get(self.find_sel).map(|l| (l.name.clone(), l.line));
            self.find_active = false;
            if let Some((name, line)) = target {
                self.doc.clear_selection();
                self.doc.set_caret(line, 0);
                self.ensure_caret_visible();
                self.after_caret();
                self.notice = format!("went to {name} (line {})", line + 1);
            }
            self.invalidate();
        }

        /// First visible match index (scroll so the selection stays in view).
        fn find_first_visible(&self) -> usize {
            self.find_sel.saturating_sub(FIND_MAX_VIS - 1)
        }

        /// The palette box `(x, y0, width)` within an editor pane of width `split`.
        fn find_layout(&self, split: f32) -> (f32, f32, f32) {
            let w = (split - 60.0).clamp(180.0, 460.0);
            (((split - w) * 0.5).max(8.0), 54.0, w)
        }

        /// Draw the palette: a filter header over a colour-coded list — blue for
        /// code labels, green for data — with the selected row highlighted.
        unsafe fn draw_find(&self, t: &ID2D1RenderTarget, split: f32, _vh: f32) {
            let (x, y0, w) = self.find_layout(split);
            let matches = self.find_matches();
            let first = self.find_first_visible();
            let vis = matches.len().saturating_sub(first).min(FIND_MAX_VIS).max(1);
            let box_h = FIND_HEADER_H + vis as f32 * FIND_ITEM_H + 6.0;

            // Accent frame + panel + header band.
            render::fill_rect(t, x - 1.0, y0 - 1.0, w + 2.0, box_h + 2.0, SPLIT_GLOW);
            render::fill_rect(t, x, y0, w, box_h, PALETTE_BG);
            render::fill_rect(t, x, y0, w, FIND_HEADER_H, PALETTE_HEADER_BG);

            // Header: the query (or a hint) + a "matches/total" count.
            let (label, lcol) = if self.find_query.is_empty() {
                ("Go to label — type to filter".to_string(), theme::TEXT_DIM)
            } else {
                (self.find_query.clone(), theme::TEXT_BRIGHT)
            };
            render::draw_text(
                t, x + 10.0, y0 + 6.0, w - 96.0, FIND_HEADER_H, &label, EDITOR_FONT, EDITOR_SIZE,
                false, false, lcol, false,
            );
            if !self.find_query.is_empty() {
                let qw = render::measure_text(&self.find_query, EDITOR_FONT, EDITOR_SIZE, false, false);
                render::fill_rect(t, x + 10.0 + qw + 1.0, y0 + 7.0, 1.5, EDITOR_SIZE, CARET_COLOR);
            }
            let count = format!("{}/{}", matches.len(), self.find_labels.len());
            let cw = render::measure_text(&count, EDITOR_FONT, EDITOR_SIZE * 0.85, false, false);
            render::draw_text(
                t, x + w - cw - 10.0, y0 + 8.0, cw + 6.0, FIND_HEADER_H, &count, EDITOR_FONT,
                EDITOR_SIZE * 0.85, false, false, theme::TEXT_DIM, false,
            );

            if matches.is_empty() {
                render::draw_text(
                    t, x + 14.0, y0 + FIND_HEADER_H + 5.0, w - 20.0, FIND_ITEM_H, "no matching labels",
                    EDITOR_FONT, EDITOR_SIZE, false, true, theme::TEXT_DIM, false,
                );
                return;
            }

            let mut iy = y0 + FIND_HEADER_H + 2.0;
            for (k, lbl) in matches.iter().enumerate().skip(first).take(FIND_MAX_VIS) {
                let sel = k == self.find_sel;
                if sel {
                    render::fill_rect(t, x + 2.0, iy, w - 4.0, FIND_ITEM_H, PALETTE_SEL);
                }
                let kcol = match lbl.kind {
                    LabelKind::Code => CODE_LABEL_COL,
                    LabelKind::Data => DATA_LABEL_COL,
                };
                // kind dot + the name (coloured by kind) + the line number.
                render::fill_rect(t, x + 11.0, iy + FIND_ITEM_H * 0.5 - 4.0, 8.0, 8.0, kcol);
                render::draw_text(
                    t, x + 28.0, iy + 3.0, w - 96.0, FIND_ITEM_H, &lbl.name, EDITOR_FONT, EDITOR_SIZE,
                    sel, false, kcol, false,
                );
                let ln = format!(":{}", lbl.line + 1);
                let lw = render::measure_text(&ln, EDITOR_FONT, EDITOR_SIZE * 0.85, false, false);
                render::draw_text(
                    t, x + w - lw - 12.0, iy + 4.0, lw + 6.0, FIND_ITEM_H, &ln, EDITOR_FONT,
                    EDITOR_SIZE * 0.85, false, false, theme::TEXT_DIM, false,
                );
                iy += FIND_ITEM_H;
            }
        }

        // ── input ──────────────────────────────────────────────────────────

        /// A printable / editing character from WM_CHAR — routed to the search box
        /// when it has focus, otherwise to the editor.
        fn on_char(&mut self, ch: u32) {
            // Go-to-label palette: typing filters; Enter jumps; Esc closes.
            if self.find_active {
                match ch {
                    0x08 => {
                        self.find_query.pop();
                        self.find_sel = 0;
                    }
                    0x0d => {
                        self.find_accept();
                        return;
                    }
                    0x1b => self.find_active = false,
                    c if c >= 0x20 && c != 0x7f => {
                        if let Some(c) = char::from_u32(c) {
                            self.find_query.push(c);
                            self.find_sel = 0;
                        }
                    }
                    _ => return,
                }
                self.invalidate();
                return;
            }
            if self.search_active {
                match ch {
                    0x08 => {
                        self.search.pop();
                    }
                    0x0D => {
                        self.run_search();
                        return;
                    }
                    c if c >= 0x20 && c != 0x7f => {
                        if let Some(c) = char::from_u32(c) {
                            self.search.push(c);
                        }
                    }
                    _ => return,
                }
                self.invalidate();
                return;
            }
            // Autocomplete popup open: Tab/Enter accept the highlighted candidate.
            if !self.comp.is_empty() && (ch == 0x09 || ch == 0x0D) {
                self.accept_completion();
                return;
            }
            // Text input (Backspace/Enter/Tab arrive here too — handled here, not
            // in WM_KEYDOWN, so they aren't applied twice).
            match ch {
                0x08 => self.doc.backspace(),
                0x0D => self.doc.insert("\n"),
                0x09 => self.doc.insert("  "),
                c if c >= 0x20 && c != 0x7f => {
                    if let Some(c) = char::from_u32(c) {
                        self.doc.type_char(c); // coalesced typing → one undo step
                    }
                }
                _ => return,
            }
            self.after_edit();
            // Re-query completion after a text change; a newline ends it.
            if ch == 0x08 || (ch >= 0x20 && ch != 0x7f) {
                self.maybe_complete();
            } else {
                self.close_completion();
            }
            self.invalidate();
        }

        /// A navigation / command key from WM_KEYDOWN. Returns true if handled.
        fn on_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
            // Go-to-label palette: arrows move the selection, Esc closes; all
            // keys are swallowed (text + Enter arrive via WM_CHAR / on_char).
            if self.find_active {
                match vk {
                    VK_ESCAPE => {
                        self.find_active = false;
                        self.invalidate();
                    }
                    VK_UP if self.find_sel > 0 => {
                        self.find_sel -= 1;
                        self.invalidate();
                    }
                    VK_DOWN if self.find_sel + 1 < self.find_matches().len() => {
                        self.find_sel += 1;
                        self.invalidate();
                    }
                    _ => {}
                }
                return true;
            }
            // Search box focused: swallow keys (text comes via WM_CHAR); Esc exits.
            if self.search_active {
                if vk == VK_ESCAPE {
                    self.search_active = false;
                    self.invalidate();
                }
                return true;
            }
            // Autocomplete popup open: arrows move the selection, Esc dismisses,
            // a caret move closes it (and then proceeds).
            if !self.comp.is_empty() && !ctrl {
                match vk {
                    VK_DOWN => {
                        self.comp_sel = (self.comp_sel + 1) % self.comp.len();
                        self.invalidate();
                        return true;
                    }
                    VK_UP => {
                        self.comp_sel = (self.comp_sel + self.comp.len() - 1) % self.comp.len();
                        self.invalidate();
                        return true;
                    }
                    VK_ESCAPE => {
                        self.close_completion();
                        return true;
                    }
                    VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_PRIOR | VK_NEXT => {
                        self.close_completion();
                    }
                    _ => {}
                }
            }
            if ctrl {
                match vk {
                    VK_F => self.search_active = true,
                    VK_G => self.find_open(),
                    VK_A => self.doc.select_all(),
                    VK_C => self.clipboard_copy(),
                    VK_X => self.clipboard_cut(),
                    VK_V => self.clipboard_paste(),
                    VK_Z if shift => self.do_redo(),
                    VK_Z => self.do_undo(),
                    VK_Y => self.do_redo(),
                    VK_N => self.file_new(),
                    VK_O => self.file_open_dialog(),
                    VK_S if shift => self.file_save_as(),
                    VK_S => self.file_save(),
                    _ => return false,
                }
                self.invalidate();
                return true;
            }
            // Caret-moving keys: extend the selection with Shift, else drop it.
            if matches!(vk, VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT) {
                if shift {
                    self.doc.start_selection();
                } else {
                    self.doc.clear_selection();
                }
            }
            match vk {
                VK_LEFT => self.doc.move_left(),
                VK_RIGHT => self.doc.move_right(),
                VK_UP => self.doc.move_up(),
                VK_DOWN => self.doc.move_down(),
                VK_HOME => self.doc.home(),
                VK_END => self.doc.end(),
                VK_PRIOR => {
                    self.page_move(false);
                    return true;
                }
                VK_NEXT => {
                    self.page_move(true);
                    return true;
                }
                VK_DELETE => return self.edit(|d| d.delete_forward()),
                VK_F5 => {
                    self.build_exe();
                    return true;
                }
                VK_F6 => {
                    self.run_exe();
                    return true;
                }
                VK_F12 => {
                    self.notice = match self.snapshot(Path::new(".")) {
                        Ok(p) => format!("saved snapshot {}", p.display()),
                        Err(e) => format!("snapshot failed: {e:#}"),
                    };
                    self.invalidate();
                    return true;
                }
                _ => return false,
            }
            // Fell through here = a pure caret move: card only, no re-assemble.
            self.after_caret();
            self.invalidate();
            true
        }

        fn clipboard_copy(&self) {
            if let Some(text) = self.doc.copy() {
                unsafe { clipboard_set(self.hwnd, &text) };
            }
        }
        fn clipboard_cut(&mut self) {
            if let Some(text) = self.doc.cut() {
                unsafe { clipboard_set(self.hwnd, &text) };
                self.after_edit();
            }
        }
        fn clipboard_paste(&mut self) {
            if let Some(text) = unsafe { clipboard_get(self.hwnd) } {
                self.doc.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
                self.after_edit();
            }
        }
        fn do_undo(&mut self) {
            if self.doc.undo() {
                self.after_edit();
            }
        }
        fn do_redo(&mut self) {
            if self.doc.redo() {
                self.after_edit();
            }
        }

        // ── menu + file commands ─────────────────────────────────────────────

        unsafe fn build_menu(&mut self) {
            let bar = CreateMenu().unwrap_or_default();
            let file = CreatePopupMenu().unwrap_or_default();
            self.recent_menu = CreatePopupMenu().unwrap_or_default();
            let edit = CreatePopupMenu().unwrap_or_default();
            let asm = CreatePopupMenu().unwrap_or_default();

            menu_str(file, IDM_NEW, "&New\tCtrl+N");
            menu_str(file, IDM_OPEN, "&Open…\tCtrl+O");
            menu_popup(file, self.recent_menu, "Open &Recent");
            menu_sep(file);
            menu_str(file, IDM_SAVE, "&Save\tCtrl+S");
            menu_str(file, IDM_SAVEAS, "Save &As…\tCtrl+Shift+S");
            menu_str(file, IDM_EXPORT, "&Export Listing…");
            menu_sep(file);
            menu_str(file, IDM_EXIT, "E&xit");

            menu_str(edit, IDM_UNDO, "&Undo\tCtrl+Z");
            menu_str(edit, IDM_REDO, "&Redo\tCtrl+Y");
            menu_sep(edit);
            menu_str(edit, IDM_CUT, "Cu&t\tCtrl+X");
            menu_str(edit, IDM_COPY, "&Copy\tCtrl+C");
            menu_str(edit, IDM_PASTE, "&Paste\tCtrl+V");
            menu_str(edit, IDM_SELALL, "Select &All\tCtrl+A");

            menu_str(asm, IDM_BUILD, "&Build .exe\tF5");
            menu_str(asm, IDM_RUN, "&Run\tF6");

            menu_popup(bar, file, "&File");
            menu_popup(bar, edit, "&Edit");
            menu_popup(bar, asm, "&Assembler");
            let _ = SetMenu(self.hwnd, Some(bar));
            self.rebuild_recent_menu();
            let _ = DrawMenuBar(self.hwnd);
        }

        unsafe fn rebuild_recent_menu(&self) {
            while GetMenuItemCount(Some(self.recent_menu)) > 0 {
                let _ = DeleteMenu(self.recent_menu, 0, MF_BYPOSITION);
            }
            if self.recent.is_empty() {
                menu_str(self.recent_menu, 0, "(none)");
                return;
            }
            for (i, p) in self.recent.iter().take(9).enumerate() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                menu_str(self.recent_menu, IDM_RECENT_BASE + i as u16, &format!("&{} {name}", i + 1));
            }
        }

        /// Dispatch a menu command (WM_COMMAND low word).
        fn on_command(&mut self, id: u16) {
            match id {
                IDM_NEW => self.file_new(),
                IDM_OPEN => self.file_open_dialog(),
                IDM_SAVE => self.file_save(),
                IDM_SAVEAS => self.file_save_as(),
                IDM_EXPORT => self.export_listing(),
                IDM_EXIT => unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
                },
                IDM_UNDO => self.do_undo(),
                IDM_REDO => self.do_redo(),
                IDM_CUT => self.clipboard_cut(),
                IDM_COPY => self.clipboard_copy(),
                IDM_PASTE => self.clipboard_paste(),
                IDM_SELALL => {
                    self.doc.select_all();
                    self.invalidate();
                }
                IDM_BUILD => self.build_exe(),
                IDM_RUN => self.run_exe(),
                id if id >= IDM_RECENT_BASE => {
                    if let Some(p) = self.recent.get((id - IDM_RECENT_BASE) as usize).cloned() {
                        self.file_open(&p);
                    }
                }
                _ => {}
            }
        }

        fn file_new(&mut self) {
            self.doc = Doc::from_str("");
            self.path = None;
            self.editor_scroll = 0.0;
            self.after_edit();
            self.update_title();
            self.invalidate();
        }

        fn file_open(&mut self, path: &Path) {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    self.doc = Doc::from_str(&text);
                    self.path = Some(path.to_path_buf());
                    self.editor_scroll = 0.0;
                    self.add_recent(path);
                    self.after_edit();
                    self.update_title();
                }
                Err(e) => self.notice = format!("open failed: {e}"),
            }
            self.invalidate();
        }

        fn file_open_dialog(&mut self) {
            if let Some(p) = unsafe { file_dialog(self.hwnd, false, "was") } {
                self.file_open(&p);
            }
        }

        fn file_save(&mut self) {
            match self.path.clone() {
                Some(p) => self.write_to(&p),
                None => self.file_save_as(),
            }
        }

        fn file_save_as(&mut self) {
            if let Some(p) = unsafe { file_dialog(self.hwnd, true, "was") } {
                self.write_to(&p);
                self.path = Some(p.clone());
                self.add_recent(&p);
                self.update_title();
            }
        }

        fn write_to(&mut self, path: &Path) {
            self.notice = match std::fs::write(path, self.doc.text()) {
                Ok(()) => format!("saved {}", path.display()),
                Err(e) => format!("save failed: {e}"),
            };
            self.invalidate();
        }

        fn export_listing(&mut self) {
            if let Some(p) = unsafe { file_dialog(self.hwnd, true, "lst") } {
                self.notice = match std::fs::write(&p, self.listing_text()) {
                    Ok(()) => format!("exported listing to {}", p.display()),
                    Err(e) => format!("export failed: {e}"),
                };
                self.invalidate();
            }
        }

        /// A plain-text assembler listing: each source line, then its expansion.
        fn listing_text(&self) -> String {
            let mut out = String::new();
            for row in 0..self.doc.line_count() {
                out.push_str(&format!("{:>4}  {}\n", row + 1, self.doc.line(row)));
                if self.is_macro(row) {
                    for (bytes, mask, asm) in self.listing(row) {
                        out.push_str(&format!("      {:<26}{asm}\n", hex_masked(bytes, mask)));
                    }
                }
            }
            out
        }

        /// Build the buffer and launch it. A console program gets its own
        /// console; a windowed one just opens. Run with the exe's own directory
        /// as the working dir so files it writes (a .bmp, say) land beside it.
        fn run_exe(&mut self) {
            let Some(lang) = self.lang.as_ref() else { return };
            let out = self.output_exe_path();
            let src = match self.build_source() {
                Ok(s) => s,
                Err(e) => {
                    self.notice = format!("include error: {e}");
                    self.invalidate();
                    return;
                }
            };
            self.notice = match lang.assemble(&src, Emit::Exe) {
                Some(Response::Assembled { bytes, .. }) => match std::fs::write(&out, &bytes) {
                    Ok(()) => {
                        let mut cmd = std::process::Command::new(&out);
                        if let Some(dir) = out.parent() {
                            cmd.current_dir(dir);
                        }
                        match cmd.spawn() {
                            Ok(_) => format!("running {}", out.display()),
                            Err(e) => format!("launch failed: {e}"),
                        }
                    }
                    Err(e) => format!("write failed: {e}"),
                },
                Some(Response::Error { message, .. }) => format!("build error: {message}"),
                _ => "build failed: no reply from the assembler (timed out)".to_string(),
            };
            self.invalidate();
        }

        fn add_recent(&mut self, path: &Path) {
            let p = path.to_path_buf();
            self.recent.retain(|x| x != &p);
            self.recent.insert(0, p);
            self.recent.truncate(9);
            save_recent(&self.recent);
            unsafe {
                self.rebuild_recent_menu();
                let _ = DrawMenuBar(self.hwnd);
            }
        }

        fn update_title(&self) {
            let name = self
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "untitled".to_string());
            let title = wide(&format!("RASM Studio — {name}"));
            unsafe {
                let _ = SetWindowTextW(self.hwnd, PCWSTR(title.as_ptr()));
            }
        }

        /// Apply a text edit to the document, then re-check/re-assemble + repaint.
        fn edit(&mut self, f: impl FnOnce(&mut Doc)) -> bool {
            f(&mut self.doc);
            self.after_edit();
            self.invalidate();
            true
        }

        fn scroll_card(&mut self, dips: f32) {
            let prev = self.card_scroll;
            self.card_scroll = (self.card_scroll + dips).clamp(0.0, self.card_max_scroll);
            if (self.card_scroll - prev).abs() > 0.01 {
                self.invalidate();
            }
        }

        // ── editor scrolling ─────────────────────────────────────────────────

        /// Visible height of the editor pane in DIPs (above the output pane).
        fn editor_viewport_h(&self) -> f32 {
            self.split_y(self.viewport().1).max(0.0)
        }
        /// Total height of all rows (including macro expansions), plus a margin.
        fn editor_content_h(&self) -> f32 {
            self.row_layout().last().map(|&(t, h)| t + h).unwrap_or(TOP_PAD) + TOP_PAD
        }
        fn clamp_editor_scroll(&mut self) {
            let max = (self.editor_content_h() - self.editor_viewport_h()).max(0.0);
            self.editor_scroll = self.editor_scroll.clamp(0.0, max);
        }
        fn scroll_editor(&mut self, dips: f32) {
            let prev = self.editor_scroll;
            self.editor_scroll += dips;
            self.clamp_editor_scroll();
            if (self.editor_scroll - prev).abs() > 0.01 {
                self.invalidate();
            }
        }

        /// Scroll just enough to keep the caret's row in view (called after any
        /// caret move; the wheel is free to scroll away between moves).
        fn ensure_caret_visible(&mut self) {
            let vh = self.editor_viewport_h();
            if vh <= 1.0 {
                return; // no viewport yet (pre first paint)
            }
            let rows = self.row_layout();
            if let Some(&(top, h)) = rows.get(self.doc.caret.row) {
                if top < self.editor_scroll {
                    self.editor_scroll = top;
                } else if top + h > self.editor_scroll + vh {
                    self.editor_scroll = top + h - vh;
                }
                self.clamp_editor_scroll();
            }
        }

        /// Move the caret up/down by about one viewport of rows (skipping the
        /// ghost rows — the caret only ever lands on a source line).
        fn page_move(&mut self, down: bool) {
            let rows = self.row_layout();
            let vh = self.editor_viewport_h().max(LINE_H);
            let mut r = self.doc.caret.row;
            let mut acc = 0.0;
            while acc < vh {
                if down {
                    if r + 1 >= rows.len() {
                        break;
                    }
                    acc += rows[r].1;
                    r += 1;
                } else {
                    if r == 0 {
                        break;
                    }
                    r -= 1;
                    acc += rows[r].1;
                }
            }
            self.doc.set_caret(r, self.doc.caret.col);
            self.after_caret();
            self.invalidate();
        }

        /// A mouse press: position the editor caret (starting a drag-selection),
        /// focus the search box, or follow a card link — by pane. Shift extends
        /// the selection from the current caret. Ctrl+click over an equate jumps
        /// to its definition (go-to-definition).
        fn on_press(&mut self, x: f32, y: f32, shift: bool, ctrl: bool) {
            // Go-to-label palette open: a click on a row jumps there; a click
            // anywhere else dismisses it.
            if self.find_active {
                let split = self.split_x(self.viewport().0);
                let (px, py0, pw) = self.find_layout(split);
                let first = self.find_first_visible();
                let items_top = py0 + FIND_HEADER_H + 2.0;
                let matches_len = self.find_matches().len();
                let vis = matches_len.saturating_sub(first).min(FIND_MAX_VIS);
                let row = if y >= items_top { ((y - items_top) / FIND_ITEM_H) as usize } else { usize::MAX };
                if x >= px && x < px + pw && row < vis {
                    self.find_sel = first + row;
                    self.find_accept();
                } else {
                    self.find_active = false;
                    self.invalidate();
                }
                return;
            }
            // A splitter toggle collapses or reveals its pane at the default size.
            if let Some(s) = self.toggle_at(x, y) {
                match s {
                    Split::Col => self.assistant_open = !self.assistant_open,
                    Split::Row => self.output_open = !self.output_open,
                }
                self.card_layout = None; // the editor/card width changed
                self.clamp_editor_scroll();
                self.invalidate();
                return;
            }
            // Pressing a splitter band begins a drag-resize.
            if let Some(s) = self.splitter_at(x, y) {
                self.drag_split = Some(s);
                return;
            }

            let (vw, vh) = self.viewport();
            let split = self.split_x(vw);
            let editor_h = self.split_y(vh);
            if self.assistant_open && x >= split {
                if y < SEARCH_H {
                    self.search_active = true;
                    self.invalidate();
                } else {
                    self.follow_card_link(x, y);
                }
            } else if x < split && y < editor_h {
                self.search_active = false;
                // Ctrl+click: if the clicked token is a defined equate, jump to
                // its definition instead of placing an editing caret.
                if ctrl {
                    let (row, col) = self.caret_at_xy(x, y);
                    if self.goto_equate(row, col) {
                        return;
                    }
                }
                if shift {
                    self.doc.start_selection();
                } else {
                    self.doc.clear_selection();
                }
                self.place_caret(x, y);
                if !shift {
                    self.doc.start_selection(); // anchor here for the drag
                }
                self.dragging = true;
            }
        }

        /// Pointer move: resize a splitter being dragged, else refresh the
        /// splitter hover (glow + resize cursor), else extend a text selection.
        fn on_mouse_move(&mut self, x: f32, y: f32) {
            if let Some(s) = self.drag_split {
                let (vw, vh) = self.viewport();
                match s {
                    Split::Col => {
                        self.split_frac = (x / vw.max(1.0)).clamp(0.05, 0.95);
                        self.assistant_open = true;
                        self.card_layout = None;
                    }
                    Split::Row => {
                        let hi = (vh - MIN_PANE).max(MIN_PANE);
                        self.output_h = (vh - y).clamp(MIN_PANE, hi);
                        self.output_open = true;
                        self.clamp_editor_scroll();
                    }
                }
                self.invalidate();
                return;
            }
            let hov = if self.dragging { None } else { self.splitter_at(x, y) };
            if hov != self.hover_split {
                self.hover_split = hov;
                self.invalidate();
            }
            self.on_drag(x, y);
        }

        /// Go-to-definition for an equate: if the token at `(row, col)` names a
        /// user-defined equate, move the caret to its definition line (1-based in
        /// the table → 0-based row) and scroll it into view. Returns whether it
        /// jumped, so the caller can fall through to normal click handling.
        fn goto_equate(&mut self, row: usize, col: usize) -> bool {
            let line = self.doc.line(row).to_string();
            let Some(tok) = studio::hover::token_at(&line, col) else { return false };
            let name = &line[tok.start..tok.end];
            let Some(def) = self.equates.iter().find(|e| e.name == name) else { return false };
            let (def_name, def_line) = (def.name.clone(), def.line);
            let target = def_line.saturating_sub(1).min(self.doc.line_count().saturating_sub(1));
            self.doc.clear_selection();
            self.doc.set_caret(target, 0);
            self.after_caret();
            self.notice = format!("jumped to `{def_name}` (line {def_line})");
            self.invalidate();
            true
        }

        /// Extend the selection by dragging (no per-move card refresh).
        fn on_drag(&mut self, x: f32, y: f32) {
            if !self.dragging {
                return;
            }
            let (row, col) = self.caret_at_xy(x, y);
            self.doc.set_caret(row, col);
            self.ensure_caret_visible();
            self.invalidate();
        }

        /// The (row, col) nearest a point in the editor's source column.
        fn caret_at_xy(&self, x: f32, y: f32) -> (usize, usize) {
            let rows = self.row_layout();
            let cy = y + self.editor_scroll; // viewport space
            let row = rows
                .iter()
                .position(|&(top, h)| cy >= top && cy < top + h)
                .unwrap_or_else(|| self.doc.line_count().saturating_sub(1));
            let line = self.doc.line(row);
            let target = x - SRC_X;
            let (mut best, mut best_d) = (0usize, f32::MAX);
            let mut idx = 0usize;
            loop {
                let d = (measure(&line[..idx]) - target).abs();
                if d < best_d {
                    best_d = d;
                    best = idx;
                }
                if idx >= line.len() {
                    break;
                }
                idx += line[idx..].chars().next().unwrap().len_utf8();
            }
            (row, best)
        }

        /// Place the caret nearest a click, refreshing the card for its symbol.
        fn place_caret(&mut self, x: f32, y: f32) {
            let (row, col) = self.caret_at_xy(x, y);
            self.doc.set_caret(row, col);
            self.after_caret();
            self.invalidate();
        }

        /// Follow a `was:` link in the card region.
        fn follow_card_link(&mut self, x: f32, y: f32) {
            let Some(layout) = self.card_layout.as_ref() else { return };
            let dy = y + self.card_scroll;
            let href = layout
                .hits
                .iter()
                .find(|h| x >= h.x0 && x <= h.x1 && dy >= h.y0 && dy <= h.y1)
                .map(|h| h.href.clone());
            if let Some(href) = href {
                if let Some(target) = studio::nav_target(&href) {
                    self.card_word.clear(); // force a fresh card even for the same word
                    self.refresh_card(Some(target.to_string()));
                    self.invalidate();
                }
            }
        }
    }

    /// The identifier under the caret worth a card (a function / constant /
    /// struct name) — `None` for registers, numbers, punctuation, keywords.
    fn lang_word(doc: &Doc) -> Option<String> {
        let line = doc.line(doc.caret.row);
        let tok = studio::hover::token_at(line, doc.caret.col)?;
        match tok.kind {
            // Functions, struct/type names (Ident) and Windows constants — the
            // things winkb has a card for; registers → the Win64 ABI card;
            // mnemonics → the instruction/flags card. Numbers: no card.
            TokKind::Ident | TokKind::Constant | TokKind::Register | TokKind::Mnemonic => {
                Some(line[tok.start..tok.end].to_string())
            }
            _ => None,
        }
    }

    fn welcome_card() -> String {
        "# RASM Studio\n\nType assembly on the left. As the caret moves over a \
         **function**, **constant**, or **struct**, its card appears here.\n\n\
         The status bar shows the **machine-code bytes** of the caret's line. \
         Press **F5** to assemble the whole buffer to a self-contained `.exe`.\n\n\
         Try moving the caret onto `ExitProcess`.\n"
            .to_string()
    }

    // ── TCL UI scripting: drive the IDE for headless, screenshot-able UI tests ──
    //
    // An embedded TCL interpreter runs a script against a (windowless) `App`,
    // with verbs that type, move/click/drag the mouse, set the splitters, open
    // files, screenshot to PNG, read back state, and assert. The VM runs
    // synchronously on this thread, so verbs reach the `App` through a
    // thread-local pointer set for the duration of `eval` — no locking, and the
    // `App` need not be `Send`.

    thread_local! {
        static SCRIPT_APP: std::cell::Cell<*mut App> = std::cell::Cell::new(std::ptr::null_mut());
    }

    /// Run `f` against the script's bound `App` (valid only while a script runs).
    fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> R {
        let p = SCRIPT_APP.with(|c| c.get());
        assert!(!p.is_null(), "no App bound for the TCL script");
        f(unsafe { &mut *p })
    }

    fn num(a: &[Value], i: usize) -> Result<f32, TclError> {
        a.get(i)
            .and_then(|v| v.as_str().parse::<f32>().ok())
            .ok_or_else(|| TclError::runtime(format!("argument {} must be a number", i + 1)))
    }
    fn uint(a: &[Value], i: usize) -> Result<usize, TclError> {
        a.get(i)
            .and_then(|v| v.as_str().parse::<usize>().ok())
            .ok_or_else(|| TclError::runtime(format!("argument {} must be an integer", i + 1)))
    }
    fn boolstr(b: bool) -> Value {
        Value::new(if b { "1" } else { "0" })
    }

    /// Map a key name (optionally `Ctrl+`/`Shift+` prefixed) to a virtual key +
    /// modifiers, for the `key` verb.
    fn map_key(spec: &str) -> Option<(VIRTUAL_KEY, bool, bool)> {
        let (mut ctrl, mut shift, mut name) = (false, false, spec);
        for part in spec.split('+') {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                _ => name = part,
            }
        }
        let code: u16 = match name.to_ascii_lowercase().as_str() {
            "enter" | "return" => 0x0D,
            "backspace" | "back" => 0x08,
            "delete" | "del" => 0x2E,
            "tab" => 0x09,
            "escape" | "esc" => 0x1B,
            "left" => 0x25,
            "up" => 0x26,
            "right" => 0x27,
            "down" => 0x28,
            "home" => 0x24,
            "end" => 0x23,
            "pageup" | "prior" => 0x21,
            "pagedown" | "next" => 0x22,
            s if s.len() == 1 => s.chars().next().unwrap().to_ascii_uppercase() as u16,
            _ => return None,
        };
        Some((VIRTUAL_KEY(code), ctrl, shift))
    }

    /// The WM_CHAR a key produces, so the `key` verb mirrors Windows (a
    /// char-yielding key generates both WM_KEYDOWN and WM_CHAR). `None` for keys
    /// that produce no character (arrows, Home, F-keys, ...).
    fn key_char(vk: VIRTUAL_KEY, shift: bool) -> Option<u32> {
        Some(match vk.0 {
            0x0D => 0x0D, // Enter
            0x08 => 0x08, // Backspace
            0x09 => 0x09, // Tab
            0x1B => 0x1B, // Escape
            c @ 0x41..=0x5A => {
                let ch = c as u8 as char;
                if shift { ch as u32 } else { ch.to_ascii_lowercase() as u32 }
            }
            c @ 0x30..=0x39 => c as u32, // digits
            _ => return None,
        })
    }

    fn set_pane(pane: &str, open: bool) -> Result<Value, TclError> {
        match pane {
            "assistant" => with_app(|app| {
                app.assistant_open = open;
                app.card_layout = None;
            }),
            "output" => with_app(|app| {
                app.output_open = open;
                app.clamp_editor_scroll();
            }),
            other => return Err(TclError::runtime(format!("unknown pane: {other} (assistant|output)"))),
        }
        Ok(Value::new(""))
    }

    /// TCL core verbs (`set`/`if`/`expr`/`foreach`/…) plus the IDE-driving verbs.
    fn script_registry() -> Registry {
        let mut r = Registry::with_core();

        // input
        r.register("type", Arity::exact(1), |_, a| {
            let text = a[0].as_str().to_string();
            with_app(|app| {
                for ch in text.chars() {
                    app.on_char(ch as u32);
                }
            });
            Ok(Value::new(""))
        });
        r.register("key", Arity::exact(1), |_, a| {
            let (vk, ctrl, shift) = map_key(a[0].as_str())
                .ok_or_else(|| TclError::runtime(format!("unknown key: {}", a[0].as_str())))?;
            with_app(|app| {
                app.on_key(vk, ctrl, shift);
                // Mirror Windows: a char-producing key also yields WM_CHAR (so
                // Enter/typing reach on_char). Ctrl combos produce no usable char.
                if !ctrl {
                    if let Some(ch) = key_char(vk, shift) {
                        app.on_char(ch);
                    }
                }
            });
            Ok(Value::new(""))
        });
        r.register("caret", Arity::exact(2), |_, a| {
            let (row, col) = (uint(a, 0)?, uint(a, 1)?);
            with_app(|app| {
                app.doc.set_caret(row, col);
                app.after_caret();
            });
            Ok(Value::new(""))
        });

        // pointer
        r.register("move", Arity::exact(2), |_, a| {
            let (x, y) = (num(a, 0)?, num(a, 1)?);
            with_app(|app| {
                app.last_mouse = (x, y);
                app.on_mouse_move(x, y);
            });
            Ok(Value::new(""))
        });
        r.register("click", Arity::exact(2), |_, a| {
            let (x, y) = (num(a, 0)?, num(a, 1)?);
            with_app(|app| {
                app.on_press(x, y, false, false);
                app.dragging = false;
                app.drag_split = None;
            });
            Ok(Value::new(""))
        });
        r.register("drag", Arity::exact(4), |_, a| {
            let (x1, y1, x2, y2) = (num(a, 0)?, num(a, 1)?, num(a, 2)?, num(a, 3)?);
            with_app(|app| {
                app.on_press(x1, y1, false, false);
                app.last_mouse = (x2, y2);
                app.on_mouse_move(x2, y2);
                app.dragging = false;
                app.drag_split = None;
            });
            Ok(Value::new(""))
        });

        // panes / splitters
        r.register("splitter", Arity::exact(2), |_, a| {
            let v = num(a, 1)?;
            match a[0].as_str() {
                "col" => with_app(|app| {
                    app.split_frac = v.clamp(0.05, 0.95);
                    app.assistant_open = true;
                    app.card_layout = None;
                }),
                "row" => with_app(|app| {
                    app.output_h = v.max(0.0);
                    app.output_open = true;
                    app.clamp_editor_scroll();
                }),
                other => return Err(TclError::runtime(format!("splitter: expected col|row, got {other}"))),
            }
            Ok(Value::new(""))
        });
        r.register("collapse", Arity::exact(1), |_, a| set_pane(a[0].as_str(), false));
        r.register("expand", Arity::exact(1), |_, a| set_pane(a[0].as_str(), true));

        // files / window
        r.register("open", Arity::exact(1), |_, a| {
            let p = PathBuf::from(a[0].as_str());
            with_app(|app| app.file_open(&p));
            Ok(Value::new(""))
        });
        r.register("new", Arity::exact(0), |_, _| {
            with_app(|app| app.file_new());
            Ok(Value::new(""))
        });
        r.register("size", Arity::exact(2), |_, a| {
            let (w, h) = (uint(a, 0)? as u32, uint(a, 1)? as u32);
            with_app(|app| {
                app.client_w = w;
                app.client_h = h;
                app.card_layout = None;
            });
            Ok(Value::new(""))
        });

        // screenshot
        r.register("screenshot", Arity::exact(1), |_, a| {
            let p = PathBuf::from(a[0].as_str());
            with_app(|app| app.render_png(&p))
                .map_err(|e| TclError::runtime(format!("screenshot: {e:#}")))?;
            Ok(Value::new(a[0].as_str()))
        });

        // state read-back (for assertions)
        r.register("text", Arity::exact(0), |_, _| Ok(Value::new(with_app(|app| app.doc.text()))));
        r.register("line", Arity::exact(1), |_, a| {
            let n = uint(a, 0)?;
            Ok(Value::new(with_app(|app| app.doc.line(n).to_string())))
        });
        r.register("linecount", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| app.doc.line_count().to_string())))
        });
        r.register("caret-row", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| app.doc.caret.row.to_string())))
        });
        r.register("caret-col", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| app.doc.caret.col.to_string())))
        });
        r.register("notice", Arity::exact(0), |_, _| Ok(Value::new(with_app(|app| app.notice.clone()))));
        r.register("split-frac", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| format!("{:.4}", app.split_frac))))
        });
        r.register("output-h", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| format!("{:.1}", app.output_h))))
        });
        r.register("assistant-open", Arity::exact(0), |_, _| Ok(boolstr(with_app(|app| app.assistant_open))));
        r.register("output-open", Arity::exact(0), |_, _| Ok(boolstr(with_app(|app| app.output_open))));
        r.register("hover-split", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| match app.hover_split {
                Some(Split::Col) => "col",
                Some(Split::Row) => "row",
                None => "none",
            })))
        });

        // go-to-label palette
        r.register("find-active", Arity::exact(0), |_, _| Ok(boolstr(with_app(|app| app.find_active))));
        r.register("find-query", Arity::exact(0), |_, _| Ok(Value::new(with_app(|app| app.find_query.clone()))));
        r.register("find-count", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| app.find_matches().len().to_string())))
        });
        r.register("find-selected", Arity::exact(0), |_, _| {
            Ok(Value::new(with_app(|app| {
                app.find_matches().get(app.find_sel).map(|l| l.name.clone()).unwrap_or_default()
            })))
        });

        // assertions (a failure aborts the script with a non-zero exit)
        r.register("assert", Arity::range(1, 2), |_, a| {
            let v = a[0].as_str();
            if v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false") {
                Err(TclError::runtime(
                    a.get(1).map(|m| m.as_str().to_string()).unwrap_or_else(|| format!("assertion failed: {v}")),
                ))
            } else {
                Ok(Value::new(""))
            }
        });
        r.register("assert-eq", Arity::range(2, 3), |_, a| {
            let (x, y) = (a[0].as_str(), a[1].as_str());
            if x == y {
                Ok(Value::new(""))
            } else {
                let pfx = a.get(2).map(|m| format!("{}: ", m.as_str())).unwrap_or_default();
                Err(TclError::runtime(format!("{pfx}expected `{y}`, got `{x}`")))
            }
        });

        r
    }

    /// Run a TCL `source` script against `app`, returning its captured output.
    fn run_script(source: &str, app: &mut App) -> anyhow::Result<String> {
        let registry = script_registry();
        SCRIPT_APP.with(|c| c.set(app as *mut App));
        let result = rust_tcl::eval(source, &registry);
        SCRIPT_APP.with(|c| c.set(std::ptr::null_mut()));
        result.map(|r| r.output).map_err(|e| anyhow::anyhow!("{e}"))
    }

    const HELP: &str = "\
RASM Studio — the WRASM x86-64 assembler IDE (Direct2D).

USAGE
  studio                       open the IDE window
  studio --shot [dir]          render one offscreen frame to a PNG, then exit
  studio --script <file.tcl>   run a TCL UI script headlessly, then exit
  studio --exec \"<tcl>\"        run an inline TCL script, then exit
  studio --help | -h           this help

  The knowledge DB ($WINKB_DB, else E:\\windows_api\\windows_api.db) backs the
  assistant cards, autocomplete, and invoke/struct/COM resolution.

TCL UI SCRIPTING (--script / --exec)
  An embedded TCL interpreter drives a windowless IDE, so UI behaviour is
  scriptable and screenshots are reviewable without a desktop. Full TCL core is
  available (set / if / expr / foreach / proc / ...). Notes for scripting:
    * coordinates are in DIPs; the default headless viewport is 1100x720
    * expr is integer-only here — use literal numbers for coordinates
    * use forward slashes in paths (TCL treats backslash as an escape)
    * an assert* failure exits non-zero, so scripts double as UI tests
    * editor shortcuts: Ctrl+G = go-to-label palette, Ctrl+F = Windows API search

  input     type \"text\"            type characters into the editor
            key NAME               one key: Enter Backspace Delete Tab Escape
                                   Left Right Up Down Home End PageUp PageDown
                                   or a letter; prefix Ctrl+ / Shift+ (Ctrl+S)
            caret ROW COL          place the caret (0-based row/col)
  pointer   move X Y               move the mouse (hover; lights a splitter)
            click X Y              press + release at a point
            drag X1 Y1 X2 Y2       press, move, release (resize a splitter, ...)
  panes     splitter col FRAC      set the editor|assistant split (0.0..1.0)
            splitter row PX        set the output-pane height (DIPs)
            collapse PANE          hide a pane (PANE = assistant | output)
            expand PANE            reveal it at its default size
  files     open FILE              load a .was file into the editor
            new                    start an empty buffer
            size W H               set the viewport size (pixels)
  capture   screenshot PATH.png    render the current frame to a PNG file
  read-back text                   the whole editor buffer
            line N                 the text of line N
            linecount              number of lines
            caret-row / caret-col  the caret position
            split-frac             the editor|assistant split fraction
            output-h               the output-pane height (DIPs)
            assistant-open         1/0 — is the assistant pane shown
            output-open            1/0 — is the output pane shown
            hover-split            col | row | none — splitter under the mouse
            find-active            1/0 — is the go-to-label palette open
            find-query             the palette's current filter text
            find-count             labels matching the filter
            find-selected          the highlighted label's name
            notice                 the status-line text
  assert    assert COND ?msg?      fail (exit 1) unless COND is true / non-zero
            assert-eq GOT WANT ?msg?   fail unless GOT equals WANT

  Example script: crates/studio/scripts/ui_demo.tcl
";

    pub fn run() -> anyhow::Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); // for WIC (snapshots)
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        render::init().map_err(|e| anyhow::anyhow!("docpane render init: {e}"))?;

        let db = std::env::var("WINKB_DB")
            .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());

        // Headless `--shot [dir]`: render one offscreen frame to a timestamped
        // PNG and exit — no window, so it works without a visible desktop.
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--help" || a == "-h") {
            print!("{HELP}");
            return Ok(());
        }
        if let Some(i) = args.iter().position(|a| a == "--shot") {
            let dir = args
                .get(i + 1)
                .filter(|a| !a.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let lang = Lang::spawn(&db).ok();
            let mut app = App::new(HWND::default(), lang);
            let path = app.snapshot(&dir)?;
            println!("wrote {}", path.display());
            return Ok(());
        }

        // Headless TCL UI script: `--script <file>` or `--exec "<tcl>"`. Drives a
        // windowless App (type / click / drag splitters / screenshot / assert)
        // and exits — so UI tests run from the shell and the shots are reviewable.
        let script_src = if let Some(i) = args.iter().position(|a| a == "--script") {
            let f = args.get(i + 1).ok_or_else(|| anyhow::anyhow!("--script needs a file path"))?;
            Some(std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("read {f}: {e}"))?)
        } else if let Some(i) = args.iter().position(|a| a == "--exec") {
            Some(args.get(i + 1).cloned().ok_or_else(|| anyhow::anyhow!("--exec needs a script string"))?)
        } else {
            None
        };
        if let Some(src) = script_src {
            let lang = Lang::spawn(&db).ok();
            let mut app = App::new(HWND::default(), lang);
            let out = run_script(&src, &mut app)?;
            print!("{out}");
            return Ok(());
        }

        let lang = match Lang::spawn(&db) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("[studio] language thread did not start ({e:#}); editor-only mode.");
                None
            }
        };

        let h_instance = unsafe { GetModuleHandleW(None) }?;
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance.into(),
            hIcon: Default::default(),
            hCursor: cursor,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: CLASS,
            hIconSm: Default::default(),
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            anyhow::bail!("RegisterClassExW returned 0");
        }

        // Hand the language thread to the window via the CreateWindowEx lparam;
        // the WM_CREATE handler adopts it and builds the per-window `App`.
        let app_seed = Box::into_raw(Box::new(AppSeed { lang }));
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("RASM Studio"),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1100,
                720,
                None,
                None,
                Some(h_instance.into()),
                Some(app_seed as *const _),
            )
        }?;

        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
        // Poll the language thread ~30×/s to apply async replies.
        let _ = unsafe { SetTimer(Some(hwnd), 1, 33, None) };

        let mut msg = MSG::default();
        unsafe {
            loop {
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 <= 0 {
                    break; // 0 = WM_QUIT, -1 = error
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }

    /// Carries the `Lang` from `run` into the window's `App` on WM_CREATE.
    struct AppSeed {
        lang: Option<Lang>,
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // Build the App on creation, adopting the language thread from the seed
        // pointer we passed as the CreateWindowEx lparam.
        if msg == WM_CREATE {
            let cs = lparam.0 as *const CREATESTRUCTW;
            let seed_ptr = unsafe { (*cs).lpCreateParams } as *mut AppSeed;
            let lang = if seed_ptr.is_null() {
                None
            } else {
                unsafe { Box::from_raw(seed_ptr) }.lang
            };
            let app = Box::new(App::new(hwnd, lang));
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize) };
            return LRESULT(0);
        }

        let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
        if state_ptr.is_null() {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        let app = unsafe { &mut *state_ptr };
        let scale = app.dip_scale();

        match msg {
            WM_TIMER => {
                app.poll_lang();
                LRESULT(0)
            }
            WM_COMMAND => {
                app.on_command((wparam.0 & 0xFFFF) as u16);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1), // we paint the whole client in WM_PAINT
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let _ = unsafe { BeginPaint(hwnd, &mut ps) };
                app.paint();
                let _ = unsafe { EndPaint(hwnd, &ps) };
                LRESULT(0)
            }
            WM_SIZE => {
                app.client_w = (lparam.0 & 0xFFFF) as u32;
                app.client_h = ((lparam.0 >> 16) & 0xFFFF) as u32;
                if let Some(t) = app.target.as_ref() {
                    let _ = unsafe {
                        t.Resize(&D2D_SIZE_U { width: app.client_w, height: app.client_h })
                    };
                }
                app.card_layout = None; // width tracks the pane
                app.invalidate();
                LRESULT(0)
            }
            WM_CHAR => {
                app.on_char(wparam.0 as u32);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if app.on_key(VIRTUAL_KEY(wparam.0 as u16), ctrl, shift) {
                    LRESULT(0)
                } else {
                    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
                }
            }
            WM_SETCURSOR => {
                // Over a splitter, show the resize cursor; otherwise the default.
                if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                    if let Some(s) = app.hover_split {
                        let id = match s {
                            Split::Col => IDC_SIZEWE,
                            Split::Row => IDC_SIZENS,
                        };
                        if let Ok(c) = unsafe { LoadCursorW(None, id) } {
                            unsafe { SetCursor(Some(c)) };
                        }
                        return LRESULT(1);
                    }
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_MOUSEMOVE => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 * scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 * scale;
                app.last_mouse = (x, y);
                app.on_mouse_move(x, y);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
                let dips = -(delta / 120.0) * WHEEL_STEP;
                // The wheel scrolls whichever pane the pointer is over.
                if app.assistant_open && app.last_mouse.0 >= app.split_x(app.viewport().0) {
                    app.scroll_card(dips);
                } else {
                    app.scroll_editor(dips);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 * scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 * scale;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                app.on_press(x, y, shift, ctrl);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                app.dragging = false;
                app.drag_split = None;
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let app = unsafe { Box::from_raw(state_ptr) };
                drop(app); // drops Lang → shuts the worker down cleanly
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }
}
