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
        GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassExW, SetTimer,
        SetWindowLongPtrW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA,
        IDC_ARROW, MSG, SW_SHOW, WINDOW_EX_STYLE, WM_CHAR, WM_CREATE, WM_DESTROY, WM_ERASEBKGND,
        WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY,
        WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSEXW, WNDCLASS_STYLES, WS_CLIPCHILDREN,
        WS_OVERLAPPEDWINDOW, WS_VISIBLE,
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

    use studio::diagnostics;
    use studio::doc::Doc;
    use studio::lang::{Diag, Emit, Lang, ListingRow, Response};
    use studio::syntax::TokKind;

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
    const OUTPUT_H: f32 = 120.0; // bottom-left output pane

    const SQUIGGLE: u32 = 0xF1_4C_4C; // VS Code error red
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

        // ── language-thread refresh (synchronous; sub-ms on a small buffer) ──

        /// After a text edit: (re)request a check and the per-line listing
        /// asynchronously, then update the caret-driven card. Replies arrive on
        /// the poll timer; superseded ones are dropped by id.
        fn after_edit(&mut self) {
            if let Some(lang) = self.lang.as_ref() {
                let text = self.doc.text();
                self.pending_check = lang.post_check(&text);
                self.pending_listing = lang.post_listing(&text);
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
                        if let Some(lang) = self.lang.as_ref() {
                            self.pending_card = lang.post_card(&word);
                        }
                        self.card_word = word; // tentative; the reply fills card_md
                    }
                }
            }
            self.ensure_caret_visible();
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

        /// Assemble the whole buffer to a self-contained exe and report.
        fn build_exe(&mut self) {
            let Some(lang) = self.lang.as_ref() else { return };
            let out = self.output_exe_path();
            self.notice = match lang.assemble(&self.doc.text(), Emit::Exe) {
                Some(Response::Assembled { bytes, info, .. }) => match std::fs::write(&out, &bytes) {
                    Ok(()) => format!("built {} — {info}", out.display()),
                    Err(e) => format!("build ok ({info}) but write failed: {e}"),
                },
                Some(Response::Error { message, .. }) => format!("build error: {message}"),
                _ => return,
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
            let split = vw * SPLIT_FRAC;
            let editor_h = (vh - OUTPUT_H).max(0.0);

            // Assistant card: laid out below the search band, right of the split.
            let card_x = split + theme::H_PAD;
            let card_w = (vw - card_x - theme::H_PAD).max(80.0);
            self.relayout_card(card_x, card_w, SEARCH_H + 4.0);
            let total = self.card_layout.as_ref().map(|l| l.total_h).unwrap_or(0.0);
            self.card_max_scroll = (total - vh).max(0.0);
            self.card_scroll = self.card_scroll.min(self.card_max_scroll);

            target.BeginDraw();
            let bg = theme::hex(theme::BG);
            target.Clear(Some(std::ptr::addr_of!(bg)));

            self.draw_editor(target, split, editor_h);
            self.draw_output(target, split, editor_h, vh);

            if let Some(c) = self.card_layout.as_ref() {
                let _ = render::draw_document(target, c, self.card_scroll, vh);
            }
            self.draw_search(target, split, vw);

            // Column divider on top of everything.
            render::fill_rect(target, split, 0.0, 1.5, vh, theme::BORDER);

            let _ = target.EndDraw(None, None);
        }

        /// Render the current state into an offscreen WIC bitmap and write it to a
        /// timestamped PNG in `dir`. This re-runs the exact same `render_frame`
        /// the window uses, so the file is pixel-for-pixel what's on screen — but
        /// it needs no visible desktop, which is what makes it reviewable headless
        /// (and is the whole point of the `--shot` mode). Returns the file path.
        fn snapshot(&mut self, dir: &Path) -> anyhow::Result<PathBuf> {
            // Headless (`--shot`, no window yet): adopt a default viewport and
            // reveal the caret, so the frame matches a real first paint. A live
            // F12 keeps the window's current size and scroll.
            if self.client_w == 0 {
                self.client_w = 1100;
                self.client_h = 720;
                self.ensure_caret_visible();
            }
            let (vw, vh) = self.viewport();
            let (pw, ph) = (vw.ceil() as u32, vh.ceil() as u32);
            let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let path = dir.join(format!("studio_shot_{stamp}.png"));

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
            Ok(path)
        }

        /// Per-source-row `(top, height)` in DIPs. A row is one text line tall,
        /// plus one line per expanded instruction when it's a macro — so the
        /// `invoke` source line is followed by its lowered ghost rows.
        fn row_layout(&self) -> Vec<(f32, f32)> {
            let mut out = Vec::with_capacity(self.doc.line_count());
            let mut y = TOP_PAD;
            for row in 0..self.doc.line_count() {
                let extra = if self.is_macro(row) { self.listing(row).len() } else { 0 };
                let h = (1 + extra) as f32 * LINE_H;
                out.push((y, h));
                y += h;
            }
            out
        }

        /// Draw the editor as an ASM listing: a gray byte margin on the left,
        /// colour-coded source on the right, the macro expansion as gray ghost
        /// rows beneath a macro line, the caret, and diagnostic squiggles.
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
                    render::fill_rect(t, ux, sy + LINE_H - 2.5, uw, 2.0, SQUIGGLE);
                }

                // Macro expansion: gray ghost rows of `bytes : asm` beneath.
                if macro_row {
                    for (i, (bytes, mask, asm)) in self.listing(row).iter().enumerate() {
                        let gy = sy + (i as f32 + 1.0) * LINE_H;
                        if gy + LINE_H < 0.0 {
                            continue;
                        }
                        if gy > editor_h {
                            break;
                        }
                        render::draw_text(
                            t, LN_W + 6.0, gy, BYTES_W - 10.0, LINE_H, &hex_masked(bytes, mask),
                            EDITOR_FONT, BYTE_SIZE, false, false, BYTE_COLOR, false,
                        );
                        render::draw_text(
                            t, SRC_X + 14.0, gy, text_right - SRC_X - 14.0, LINE_H, asm,
                            EDITOR_FONT, EDITOR_SIZE * 0.92, false, true, GHOST_COLOR, false,
                        );
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
                        theme::BODY_FONT, 12.0, false, false, SQUIGGLE, false,
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

        // ── input ──────────────────────────────────────────────────────────

        /// A printable / editing character from WM_CHAR — routed to the search box
        /// when it has focus, otherwise to the editor.
        fn on_char(&mut self, ch: u32) {
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
            self.invalidate();
        }

        /// A navigation / command key from WM_KEYDOWN. Returns true if handled.
        fn on_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
            // Search box focused: swallow keys (text comes via WM_CHAR); Esc exits.
            if self.search_active {
                if vk == VK_ESCAPE {
                    self.search_active = false;
                    self.invalidate();
                }
                return true;
            }
            if ctrl {
                match vk {
                    VK_F => self.search_active = true,
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
            self.notice = match lang.assemble(&self.doc.text(), Emit::Exe) {
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
                _ => "build failed".to_string(),
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
            (self.viewport().1 - OUTPUT_H).max(0.0)
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
        /// the selection from the current caret.
        fn on_press(&mut self, x: f32, y: f32, shift: bool) {
            let (vw, vh) = self.viewport();
            let split = vw * SPLIT_FRAC;
            let editor_h = (vh - OUTPUT_H).max(0.0);
            if x >= split {
                if y < SEARCH_H {
                    self.search_active = true;
                    self.invalidate();
                } else {
                    self.follow_card_link(x, y);
                }
            } else if y < editor_h {
                self.search_active = false;
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
            // things winkb has a card for. Registers/numbers/labels: no card.
            TokKind::Ident | TokKind::Constant => Some(line[tok.start..tok.end].to_string()),
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
            WM_MOUSEMOVE => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 * scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 * scale;
                app.last_mouse = (x, y);
                app.on_drag(x, y);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
                let dips = -(delta / 120.0) * WHEEL_STEP;
                // The wheel scrolls whichever pane the pointer is over.
                if app.last_mouse.0 >= app.viewport().0 * SPLIT_FRAC {
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
                app.on_press(x, y, shift);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                app.dragging = false;
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
