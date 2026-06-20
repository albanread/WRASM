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

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
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
        VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_F12, VK_F5, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT,
        VK_TAB, VK_UP, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
        GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW,
        ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, MSG,
        SW_SHOW, WINDOW_EX_STYLE, WM_CHAR, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
        WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSEXW, WNDCLASS_STYLES,
        WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    use docpane::layout::Layout;
    use docpane::{layout as dlayout, parser, render, theme};

    use studio::diagnostics;
    use studio::doc::Doc;
    use studio::lang::{Diag, Emit, Lang, Response};
    use studio::syntax::TokKind;

    const CLASS: PCWSTR = w!("RasmStudioMain");

    // Editor metrics, in DIPs. A monospace family keeps caret/squiggle maths
    // simple and the editor crisp; DirectWrite falls back if it isn't installed.
    const EDITOR_FONT: &str = theme::CODE_FONT;
    const EDITOR_SIZE: f32 = 15.0;
    const LINE_H: f32 = EDITOR_SIZE * 1.5;
    const GUTTER_W: f32 = 54.0;
    const TOP_PAD: f32 = 10.0;
    const STATUS_H: f32 = 30.0;
    const SPLIT_FRAC: f32 = 0.56;
    const WHEEL_STEP: f32 = 48.0;

    const SQUIGGLE: u32 = 0xF1_4C_4C; // VS Code error red
    const CARET_COLOR: u32 = theme::TEXT_BRIGHT;

    const STARTER: &str = "\
.globl main
main:
  invoke ExitProcess, 42
  ret
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

    struct App {
        hwnd: HWND,
        dpi: u32,
        client_w: u32,
        client_h: u32,
        target: Option<ID2D1HwndRenderTarget>,

        lang: Option<Lang>,
        doc: Doc,
        diags: Vec<Diag>,
        line_bytes: Vec<u8>,
        status: String,

        // Assistant card.
        card_md: String,
        card_word: String,
        card_layout: Option<Layout>,
        card_laid_w: f32,
        card_scroll: f32,
        card_max_scroll: f32,
    }

    impl App {
        fn new(hwnd: HWND, lang: Option<Lang>) -> Self {
            let dpi = match unsafe { GetDpiForWindow(hwnd) } {
                0 => 96,
                d => d,
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
                line_bytes: Vec::new(),
                status: String::new(),
                card_md: welcome_card(),
                card_word: String::new(),
                card_layout: None,
                card_laid_w: -1.0,
                card_scroll: 0.0,
                card_max_scroll: 0.0,
            };
            // Land the caret on `ExitProcess` so the first card is a real one.
            app.doc.set_caret(2, 9);
            app.refresh();
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
        fn split_x(&self) -> f32 {
            self.viewport().0 * SPLIT_FRAC
        }
        fn invalidate(&self) {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }

        // ── language-thread refresh (synchronous; sub-ms on a small buffer) ──

        /// Re-run the per-edit queries: diagnostics for the buffer, live bytes
        /// for the caret's line, and a card for the symbol under the caret.
        fn refresh(&mut self) {
            let Some(lang) = self.lang.as_ref() else {
                self.status = "knowledge db not found — set WINKB_DB. Editing + highlighting only."
                    .to_string();
                return;
            };

            if let Some(Response::Check { diags, .. }) = lang.check_src(&self.doc.text()) {
                self.diags = diags;
            }

            let line = self.doc.line(self.doc.caret.row).to_string();
            self.line_bytes = match lang.line_bytes(&line) {
                Some(Response::LineBytes { bytes, .. }) => bytes,
                _ => Vec::new(),
            };

            self.refresh_card(lang_word(&self.doc));
            self.compose_status();
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

        fn compose_status(&mut self) {
            let hex = studio::bytes::hex(&self.line_bytes);
            let bytes = if self.line_bytes.is_empty() {
                "—".to_string()
            } else {
                format!("{hex}  ({} bytes)", self.line_bytes.len())
            };
            let issues = match self.diags.len() {
                0 => "no issues".to_string(),
                1 => "1 issue".to_string(),
                n => format!("{n} issues"),
            };
            self.status = format!("bytes: {bytes}     {issues}     F5: build .exe");
        }

        /// Assemble the whole buffer to a self-contained exe and report.
        fn build_exe(&mut self) {
            let Some(lang) = self.lang.as_ref() else { return };
            match lang.assemble(&self.doc.text(), Emit::Exe) {
                Some(Response::Assembled { bytes, info, .. }) => {
                    let out: PathBuf = std::env::temp_dir().join("studio_build.exe");
                    match std::fs::write(&out, &bytes) {
                        Ok(()) => self.status = format!("built {} — {info}", out.display()),
                        Err(e) => self.status = format!("build ok ({info}) but write failed: {e}"),
                    }
                }
                Some(Response::Error { message, .. }) => {
                    self.status = format!("build error: {message}");
                }
                _ => {}
            }
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

        /// (Re)lay out the assistant card for the current pane width.
        fn relayout_card(&mut self, x_base: f32, width: f32) {
            let stale = self.card_layout.is_none() || (self.card_laid_w - width).abs() > 0.5;
            if stale {
                let md = if self.card_md.trim().is_empty() {
                    welcome_card()
                } else {
                    self.card_md.clone()
                };
                let blocks = parser::parse(&md);
                self.card_layout =
                    Some(dlayout::layout(&blocks, x_base, width, 0.0, render::measure_text));
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
            let (vw, vh) = self.viewport();
            let base: &ID2D1RenderTarget = &target;
            unsafe { self.render_frame(base, vw, vh) };
        }

        /// Draw a whole frame — editor + assistant + status — into any target at
        /// a viewport size in DIPs. Shared by the live window and the offscreen
        /// [`snapshot`](App::snapshot); owns `BeginDraw`/`EndDraw`.
        unsafe fn render_frame(&mut self, target: &ID2D1RenderTarget, vw: f32, vh: f32) {
            let split = vw * SPLIT_FRAC;
            let editor_h = vh - STATUS_H;

            let card_x = split + theme::H_PAD;
            let card_w = (vw - card_x - theme::H_PAD).max(80.0);
            self.relayout_card(card_x, card_w);
            let total = self.card_layout.as_ref().map(|l| l.total_h).unwrap_or(0.0);
            self.card_max_scroll = (total - editor_h).max(0.0);
            self.card_scroll = self.card_scroll.min(self.card_max_scroll);

            target.BeginDraw();
            let bg = theme::hex(theme::BG);
            target.Clear(Some(std::ptr::addr_of!(bg)));

            self.draw_editor(target, split, editor_h);

            if let Some(c) = self.card_layout.as_ref() {
                let _ = render::draw_document(target, c, self.card_scroll, editor_h);
            }

            // Divider + status strip on top.
            render::fill_rect(target, split, 0.0, 1.5, vh, theme::BORDER);
            render::fill_rect(target, 0.0, vh - STATUS_H, vw, STATUS_H, theme::SIDEBAR_BG);
            render::fill_rect(target, 0.0, vh - STATUS_H, vw, 1.0, theme::BORDER);
            render::draw_text(
                target,
                theme::H_PAD,
                vh - STATUS_H + (STATUS_H - 16.0) * 0.5,
                vw - 2.0 * theme::H_PAD,
                16.0,
                &self.status,
                theme::BODY_FONT,
                12.5,
                false,
                false,
                theme::TEXT_DIM,
                false,
            );

            let _ = target.EndDraw(None, None);
        }

        /// Render the current state into an offscreen WIC bitmap and write it to a
        /// timestamped PNG in `dir`. This re-runs the exact same `render_frame`
        /// the window uses, so the file is pixel-for-pixel what's on screen — but
        /// it needs no visible desktop, which is what makes it reviewable headless
        /// (and is the whole point of the `--shot` mode). Returns the file path.
        fn snapshot(&mut self, dir: &Path) -> anyhow::Result<PathBuf> {
            let (vw, vh) = if self.client_w > 0 {
                self.viewport()
            } else {
                (1100.0, 720.0)
            };
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

        /// Draw the editor pane: gutter line numbers, colour-coded tokens, the
        /// caret, and diagnostic squiggles.
        fn draw_editor(&self, t: &ID2D1RenderTarget, split: f32, editor_h: f32) {
            let text_x = GUTTER_W;
            let text_right = split - 6.0;
            let rows = self.doc.line_count();
            for row in 0..rows {
                let y = TOP_PAD + row as f32 * LINE_H;
                if y > editor_h {
                    break;
                }
                let line = self.doc.line(row);

                // Gutter line number.
                unsafe {
                    render::draw_text(
                        t,
                        0.0,
                        y,
                        GUTTER_W - 10.0,
                        LINE_H,
                        &format!("{:>3}", row + 1),
                        EDITOR_FONT,
                        EDITOR_SIZE * 0.85,
                        false,
                        false,
                        theme::TEXT_DIM,
                        false,
                    );
                }

                // Coloured tokens.
                for tok in self.doc.tokens(row) {
                    let x = text_x + measure(&line[..tok.start]);
                    if x > text_right {
                        continue;
                    }
                    unsafe {
                        render::draw_text(
                            t,
                            x,
                            y,
                            text_right - x,
                            LINE_H,
                            &line[tok.start..tok.end],
                            EDITOR_FONT,
                            EDITOR_SIZE,
                            false,
                            false,
                            tok_color(tok.kind),
                            false,
                        );
                    }
                }

                // Squiggles for diagnostics on this row (1-based lines).
                for d in self.diags.iter().filter(|d| d.line == row + 1) {
                    let (s, e) = diagnostics::underline(line, d.col);
                    let ux = text_x + measure(&line[..s]);
                    let uw = (measure(&line[..e]) - measure(&line[..s])).max(3.0);
                    unsafe {
                        render::fill_rect(t, ux, y + LINE_H - 2.5, uw, 2.0, SQUIGGLE);
                    }
                }
            }

            // Caret.
            let cr = self.doc.caret;
            let cy = TOP_PAD + cr.row as f32 * LINE_H;
            if cy <= editor_h {
                let cx = text_x + measure(&self.doc.line(cr.row)[..cr.col]);
                unsafe {
                    render::fill_rect(t, cx, cy + 1.0, 1.8, LINE_H - 2.0, CARET_COLOR);
                }
            }
        }

        // ── input ──────────────────────────────────────────────────────────

        /// A printable character / editing key from WM_CHAR.
        fn on_char(&mut self, ch: u32) {
            match ch {
                0x08 => self.doc.backspace(),     // Backspace
                0x0D => self.doc.insert("\n"),    // Enter
                0x09 => self.doc.insert("  "),    // Tab → two spaces
                c if c >= 0x20 && c != 0x7f => {
                    if let Some(ch) = char::from_u32(c) {
                        self.doc.insert(&ch.to_string());
                    }
                }
                _ => return,
            }
            self.refresh();
            self.invalidate();
        }

        /// A navigation / command key from WM_KEYDOWN. Returns true if handled.
        fn on_key(&mut self, vk: VIRTUAL_KEY) -> bool {
            match vk {
                VK_LEFT => self.doc.move_left(),
                VK_RIGHT => self.doc.move_right(),
                VK_UP => self.doc.move_up(),
                VK_DOWN => self.doc.move_down(),
                VK_HOME => self.doc.home(),
                VK_END => self.doc.end(),
                VK_DELETE => {
                    // Forward-delete = step right then delete-left, except at EOL.
                    let before = self.doc.caret;
                    self.doc.move_right();
                    if self.doc.caret != before {
                        self.doc.backspace();
                    }
                }
                VK_BACK => self.doc.backspace(),
                VK_RETURN => self.doc.insert("\n"),
                VK_TAB => self.doc.insert("  "),
                VK_F5 => {
                    self.build_exe();
                    return true;
                }
                VK_F12 => {
                    self.status = match self.snapshot(Path::new(".")) {
                        Ok(p) => format!("saved snapshot {}", p.display()),
                        Err(e) => format!("snapshot failed: {e:#}"),
                    };
                    self.invalidate();
                    return true;
                }
                _ => return false,
            }
            self.refresh();
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

        /// Click in the assistant pane: follow a `was:` card link.
        fn on_click(&mut self, x: f32, y: f32) {
            let Some(layout) = self.card_layout.as_ref() else { return };
            let dy = y + self.card_scroll;
            let href = layout
                .hits
                .iter()
                .find(|h| x >= h.x0 && x <= h.x1 && dy >= h.y0 && dy <= h.y1)
                .map(|h| h.href.clone());
            if let Some(href) = href {
                if let Some(target) = studio::nav_target(&href) {
                    // Navigate the card; force a fresh card even for the same word.
                    self.card_word.clear();
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
                if app.on_key(VIRTUAL_KEY(wparam.0 as u16)) {
                    LRESULT(0)
                } else {
                    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
                }
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
                app.scroll_card(-(delta / 120.0) * WHEEL_STEP);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xFFFF) as i16 as f32 * scale;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 * scale;
                app.on_click(x, y);
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
