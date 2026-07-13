use std::sync::Arc;

use crate::mermaid;
use crate::parser::{Block, ColAlign, Inline, ToolbarItem};
use crate::theme;

/// A rendered region that responds to mouse clicks (hyperlinks).
#[derive(Clone)]
pub struct HitRegion {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub href: String,
}

/// A single draw command for the content area.
#[derive(Clone)]
pub enum DrawCmd {
    FillRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: u32,
    },
    StrokeLine {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: u32,
    },
    Text {
        x: f32,
        y: f32,
        max_w: f32,
        /// Pre-encoded UTF-16 — avoids Vec allocation in every paint call.
        text: Vec<u16>,
        /// Always a `&'static str` (theme constant) — no heap allocation.
        font: &'static str,
        size: f32,
        bold: bool,
        italic: bool,
        color: u32,
        underline: bool,
    },
    /// Render a bitmap image loaded from `path` (relative to docs dir).
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        path: String,
    },
    /// Render a mermaid diagram. Position and uniform scale are baked in here;
    /// the renderer walks the IR and applies an `(x, y) + scale` transform.
    Mermaid {
        x: f32,
        y: f32,
        scale: f32,
        graph: Arc<mermaid::Graph>,
    },
}

pub struct Layout {
    pub cmds: Vec<DrawCmd>,
    pub hits: Vec<HitRegion>,
    /// Hit regions for toolbar items (separate from content links).
    pub toolbar_hits: Vec<HitRegion>,
    pub total_h: f32,
    /// Plain-text headings paired with their y position (top of the heading
    /// block). Used by the find feature to scroll to a result.
    pub headings: Vec<(String, f32)>,
    /// Source Markdown line numbers paired with the rendered block y position.
    /// Used by `--testsnap --scrollto <line>`.
    pub source_lines: Vec<(usize, f32)>,
}

struct Ctx {
    cmds: Vec<DrawCmd>,
    hits: Vec<HitRegion>,
    toolbar_hits: Vec<HitRegion>,
    headings: Vec<(String, f32)>,
    source_lines: Vec<(usize, f32)>,
    x_base: f32,
    width: f32, // content width
    y: f32,
    indent: f32,
    /// Accurate text-width measurement supplied by the renderer.
    measure: fn(&str, &str, f32, bool, bool) -> f32,
}

impl Ctx {
    fn new(
        x_base: f32,
        width: f32,
        y_start: f32,
        measure: fn(&str, &str, f32, bool, bool) -> f32,
    ) -> Self {
        Self {
            cmds: Vec::new(),
            hits: Vec::new(),
            toolbar_hits: Vec::new(),
            headings: Vec::new(),
            source_lines: Vec::new(),
            x_base,
            width,
            y: y_start,
            indent: 0.0,
            measure,
        }
    }

    fn push(&mut self, cmd: DrawCmd) {
        self.cmds.push(cmd);
    }

    fn text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        max_w: f32,
        font: &'static str,
        size: f32,
        bold: bool,
        italic: bool,
        color: u32,
        underline: bool,
    ) {
        if text.is_empty() {
            return;
        }
        self.push(DrawCmd::Text {
            x,
            y,
            max_w,
            text: text.encode_utf16().collect(),
            font,
            size,
            bold,
            italic,
            color,
            underline,
        });
    }

    fn line_h(&self, size: f32) -> f32 {
        size * theme::LINE_EXTRA
    }

    fn x(&self) -> f32 {
        self.x_base + self.indent
    }
    fn avail_w(&self) -> f32 {
        self.width - self.indent
    }
}

pub fn layout(
    blocks: &[Block],
    x_base: f32,
    width: f32,
    y_start: f32,
    measure: fn(&str, &str, f32, bool, bool) -> f32,
) -> Layout {
    let mut ctx = Ctx::new(x_base, width, y_start, measure);
    layout_blocks(&mut ctx, blocks, 0);
    ctx.y += theme::V_PAD;
    Layout {
        cmds: ctx.cmds,
        hits: ctx.hits,
        toolbar_hits: ctx.toolbar_hits,
        total_h: ctx.y,
        headings: ctx.headings,
        source_lines: ctx.source_lines,
    }
}

fn layout_blocks(ctx: &mut Ctx, blocks: &[Block], depth: usize) {
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            ctx.y += theme::PARA_GAP;
        }
        layout_block(ctx, block, depth);
    }
}

fn layout_block(ctx: &mut Ctx, block: &Block, depth: usize) {
    match block {
        Block::Located { line, block } => {
            ctx.source_lines.push((*line, ctx.y));
            layout_block(ctx, block, depth);
        }
        Block::Heading { level, inlines } => layout_heading(ctx, *level, inlines),
        Block::Paragraph(inlines) => layout_paragraph(ctx, inlines),
        Block::CodeBlock { lang, code } => layout_code(ctx, lang, code),
        Block::Blockquote(inner) => layout_blockquote(ctx, inner, depth),
        Block::BulletList(items) => layout_list(ctx, items, false, 1, depth),
        Block::OrderedList { start, items } => {
            layout_list(ctx, items, true, *start as usize, depth)
        }
        Block::ThematicBreak => layout_rule(ctx),
        Block::Table {
            headers,
            rows,
            alignments,
        } => layout_table(ctx, headers, rows, alignments),
        Block::Toolbar(items) => layout_toolbar(ctx, items),
        Block::Mermaid {
            source,
            graph,
            error,
        } => layout_mermaid(ctx, source, graph.as_ref(), error.as_deref()),
    }
}

fn layout_mermaid(
    ctx: &mut Ctx,
    source: &str,
    graph: Option<&Arc<mermaid::Graph>>,
    error: Option<&str>,
) {
    // Failure → render as a code block + an error message so the user still
    // sees their source and the reason it didn't render.
    let Some(graph) = graph else {
        if let Some(e) = error {
            layout_paragraph(ctx, &[Inline::Italic(format!("mermaid error: {e}"))]);
        }
        layout_code(ctx, "mermaid", source);
        return;
    };

    // Uniform scale-to-fit. Never enlarge — only shrink. `0.01` floor keeps
    // truly tiny content-areas from producing a zero-height row.
    let scale = (ctx.width / graph.width()).min(1.0).max(0.01);
    let h = (graph.height() * scale).max(1.0);

    ctx.push(DrawCmd::Mermaid {
        x: ctx.x_base,
        y: ctx.y,
        scale,
        graph: graph.clone(),
    });
    ctx.y += h + theme::PARA_GAP;
}

fn layout_heading(ctx: &mut Ctx, level: u8, inlines: &[Inline]) {
    let (size, color, top_gap, bot_gap) = match level {
        1 => (theme::H1_SIZE, theme::H1, 24.0_f32, 8.0_f32),
        2 => (theme::H2_SIZE, theme::H2, 20.0, 6.0),
        3 => (theme::H3_SIZE, theme::H3, 16.0, 4.0),
        4 => (theme::H4_SIZE, theme::H4, 12.0, 3.0),
        5 => (theme::H5_SIZE, theme::H5, 10.0, 2.0),
        _ => (theme::H6_SIZE, theme::H6, 8.0, 2.0),
    };
    // Record the heading position before the top gap so scrolling lands
    // with a little breathing room above the text.
    let heading_pos_y = ctx.y;
    ctx.y += top_gap;

    // H1 and H2 get a subtle separator line below
    let text = collect_inlines_text(inlines);
    ctx.headings.push((text.clone(), heading_pos_y));
    let x = ctx.x();
    let y = ctx.y;
    let max_w = ctx.avail_w();
    ctx.text(
        &text,
        x,
        y,
        max_w,
        theme::BODY_FONT,
        size,
        level <= 3,
        false,
        color,
        false,
    );
    // A long heading wraps; reserve height for every visual row, else the next
    // block overprints it.
    let rows = wrapped_rows(ctx.measure, &text, theme::BODY_FONT, size, level <= 3, max_w);
    ctx.y += rows * ctx.line_h(size);
    ctx.y += bot_gap;

    if level <= 2 {
        let lx = ctx.x();
        let lw = ctx.avail_w();
        ctx.push(DrawCmd::StrokeLine {
            x0: lx,
            y0: ctx.y,
            x1: lx + lw,
            y1: ctx.y,
            color: theme::BORDER,
        });
        ctx.y += 1.0;
    }
}

fn layout_paragraph(ctx: &mut Ctx, inlines: &[Inline]) {
    let x = ctx.x();
    let max_w = ctx.avail_w();
    let line_h = ctx.line_h(theme::BODY_SIZE);

    if inlines.is_empty() {
        return;
    }

    // Fast path: pure plain/bold/italic text with no links or code → single text run.
    let all_same_style = inlines.iter().all(|i| match i {
        Inline::Text(_)
        | Inline::Bold(_)
        | Inline::Italic(_)
        | Inline::BoldItalic(_)
        | Inline::SoftBreak
        | Inline::HardBreak => true,
        _ => false,
    });
    let first_bold = matches!(
        inlines.first(),
        Some(Inline::Bold(_) | Inline::BoldItalic(_))
    );
    let first_italic = matches!(
        inlines.first(),
        Some(Inline::Italic(_) | Inline::BoldItalic(_))
    );
    let uniform = all_same_style
        && inlines.iter().all(|i| match i {
            Inline::Bold(_) => first_bold && !first_italic,
            Inline::Italic(_) => !first_bold && first_italic,
            Inline::BoldItalic(_) => first_bold && first_italic,
            Inline::Text(_) => !first_bold && !first_italic,
            Inline::SoftBreak | Inline::HardBreak => true,
            _ => false,
        });
    if uniform {
        let mut text = String::new();
        for i in inlines {
            match i {
                Inline::Text(t) | Inline::Bold(t) | Inline::Italic(t) | Inline::BoldItalic(t) => {
                    text.push_str(t)
                }
                Inline::SoftBreak | Inline::HardBreak => text.push(' '),
                _ => {}
            }
        }
        if text.is_empty() {
            return;
        }
        let y = ctx.y;
        ctx.text(
            &text,
            x,
            y,
            max_w,
            theme::BODY_FONT,
            theme::BODY_SIZE,
            first_bold,
            first_italic,
            theme::TEXT,
            false,
        );
        ctx.y +=
            wrapped_rows(ctx.measure, &text, theme::BODY_FONT, theme::BODY_SIZE, first_bold, max_w)
                * line_h;
        return;
    }

    // Mixed paragraph: tokenise into individual words so we can flow word-by-word.
    // Working at word granularity keeps the per-token width error small so cur_x
    // never drifts enough to cause overlap (unlike per-span flow on long spans).
    struct Word {
        text: String,
        bold: bool,
        italic: bool,
        underline: bool,
        color: u32,
        font: &'static str,
        size: f32,
        href: Option<String>,
    }

    let mut words: Vec<Word> = Vec::new();

    for inline in inlines {
        match inline {
            Inline::SoftBreak | Inline::HardBreak => {
                if let Some(last) = words.last_mut() {
                    if !last.text.ends_with(' ') {
                        last.text.push(' ');
                    }
                }
                continue;
            }
            Inline::Image { .. } => {
                continue;
            } // inline images not supported in paragraphs
            _ => {}
        }

        let (raw, bold, italic, underline, color, font, size, href) = match inline {
            Inline::Text(t) => (
                t.as_str(),
                false,
                false,
                false,
                theme::TEXT,
                theme::BODY_FONT,
                theme::BODY_SIZE,
                None,
            ),
            Inline::Bold(t) => (
                t.as_str(),
                true,
                false,
                false,
                theme::TEXT,
                theme::BODY_FONT,
                theme::BODY_SIZE,
                None,
            ),
            Inline::Italic(t) => (
                t.as_str(),
                false,
                true,
                false,
                theme::TEXT,
                theme::BODY_FONT,
                theme::BODY_SIZE,
                None,
            ),
            Inline::BoldItalic(t) => (
                t.as_str(),
                true,
                true,
                false,
                theme::TEXT,
                theme::BODY_FONT,
                theme::BODY_SIZE,
                None,
            ),
            Inline::Code(t) => (
                t.as_str(),
                false,
                false,
                false,
                theme::CODE_FG,
                theme::CODE_FONT,
                theme::CODE_SIZE,
                None,
            ),
            Inline::Link { text, href } => (
                text.as_str(),
                false,
                false,
                true,
                theme::LINK,
                theme::BODY_FONT,
                theme::BODY_SIZE,
                Some(href.clone()),
            ),
            Inline::SoftBreak | Inline::HardBreak => unreachable!(),
            Inline::Image { .. } => continue,
        };

        // Code spans stay as one token (they are short and must not be split).
        // Everything else is split on spaces so each token is at most one word.
        if matches!(inline, Inline::Code(_)) {
            words.push(Word {
                text: raw.to_string(),
                bold,
                italic,
                underline,
                color,
                font,
                size,
                href,
            });
        } else {
            // split_inclusive(' ') keeps the trailing space with the preceding word,
            // which is exactly what we want for proper inter-word spacing.
            for chunk in raw.split_inclusive(' ') {
                if chunk.is_empty() {
                    continue;
                }
                words.push(Word {
                    text: chunk.to_string(),
                    bold,
                    italic,
                    underline,
                    color,
                    font,
                    size,
                    href: href.clone(),
                });
            }
        }
    }

    if words.is_empty() {
        return;
    }

    // Flow words left-to-right, wrapping when the next word would overflow.
    let mut cur_x = x;
    let mut cur_y = ctx.y;

    for word in &words {
        let w = (ctx.measure)(&word.text, word.font, word.size, word.bold, word.italic);

        // Wrap if word doesn't fit and we are not already at the line start.
        if cur_x > x && cur_x + w > x + max_w {
            cur_x = x;
            cur_y += line_h;
        }

        let avail = (x + max_w) - cur_x;
        // Inline code uses a different font with different ascender metrics; shift it
        // down so its baseline aligns with the surrounding body text.
        let word_y = if word.font == theme::CODE_FONT {
            cur_y + theme::INLINE_CODE_Y
        } else {
            cur_y
        };
        ctx.text(
            &word.text,
            cur_x,
            word_y,
            avail,
            word.font,
            word.size,
            word.bold,
            word.italic,
            word.color,
            word.underline,
        );

        if let Some(href) = &word.href {
            ctx.hits.push(HitRegion {
                x0: cur_x,
                y0: cur_y,
                x1: cur_x + w,
                y1: cur_y + line_h,
                href: href.clone(),
            });
        }

        cur_x += w;
    }

    ctx.y = cur_y + line_h;
}

fn layout_code(ctx: &mut Ctx, _lang: &str, code: &str) {
    let pad = theme::CODE_PAD;
    let x = ctx.x();
    let w = ctx.avail_w();
    let lines: Vec<&str> = code.lines().collect();
    let line_h = ctx.line_h(theme::CODE_SIZE);
    let text_x = x + pad;
    let text_max_w = (w - pad * 2.0).max(1.0);

    // A long line wraps to several visual rows at render time, so reserve height
    // for each line's wrapped rows — otherwise the overflow overprints the next
    // line (the "scrunched code" bug).
    let rows: Vec<f32> = lines
        .iter()
        .map(|l| wrapped_rows(ctx.measure, l, theme::CODE_FONT, theme::CODE_SIZE, false, text_max_w))
        .collect();
    let total_rows: f32 = rows.iter().sum();
    let block_h = total_rows * line_h + pad * 2.0;

    ctx.push(DrawCmd::FillRect {
        x,
        y: ctx.y,
        w,
        h: block_h,
        color: theme::CODE_BG,
    });

    let mut ty = ctx.y + pad;
    for (line, &r) in lines.iter().zip(&rows) {
        ctx.text(
            line,
            text_x,
            ty,
            text_max_w,
            theme::CODE_FONT,
            theme::CODE_SIZE,
            false,
            false,
            theme::CODE_FG,
            false,
        );
        ty += r * line_h;
    }

    ctx.y += block_h;
}

fn layout_blockquote(ctx: &mut Ctx, inner: &[Block], depth: usize) {
    let bar_x = ctx.x();
    let y_start = ctx.y;

    ctx.indent += theme::BQ_BAR_W + theme::BQ_PAD;
    layout_blocks(ctx, inner, depth + 1);
    ctx.indent -= theme::BQ_BAR_W + theme::BQ_PAD;

    let y_end = ctx.y;
    ctx.push(DrawCmd::FillRect {
        x: bar_x,
        y: y_start,
        w: theme::BQ_BAR_W,
        h: y_end - y_start,
        color: theme::BLOCKQUOTE,
    });
}

fn layout_list(ctx: &mut Ctx, items: &[Vec<Inline>], ordered: bool, start: usize, _depth: usize) {
    let bullet_x = ctx.x();
    ctx.indent += 24.0;

    for (i, item_inlines) in items.iter().enumerate() {
        // Bullet or number
        let marker = if ordered {
            format!("{}.", start + i)
        } else {
            "•".to_string()
        };
        let bx = bullet_x;
        let by = ctx.y;
        ctx.text(
            &marker,
            bx,
            by,
            20.0,
            theme::BODY_FONT,
            theme::BODY_SIZE,
            false,
            false,
            theme::TEXT_DIM,
            false,
        );

        layout_paragraph(ctx, item_inlines);
        ctx.y += 2.0;
    }

    ctx.indent -= 24.0;
}

fn layout_table(ctx: &mut Ctx, headers: &[String], rows: &[Vec<String>], alignments: &[ColAlign]) {
    if headers.is_empty() {
        return;
    }
    let x = ctx.x();
    let w = ctx.avail_w();
    let col_count = headers.len();
    let cell_pad = 8.0;
    let line_h = ctx.line_h(theme::BODY_SIZE);

    // ── Column width sizing ────────────────────────────────────────────────
    let mut natural: Vec<f32> = headers
        .iter()
        .map(|h| (ctx.measure)(h, theme::BODY_FONT, theme::BODY_SIZE, true, false) + cell_pad * 2.0)
        .collect();
    for row in rows {
        for (ci, cell) in row.iter().enumerate().take(col_count) {
            let cw = (ctx.measure)(cell, theme::BODY_FONT, theme::BODY_SIZE, false, false)
                + cell_pad * 2.0;
            if cw > natural[ci] {
                natural[ci] = cw;
            }
        }
    }
    // Each column's minimum is its widest unbreakable word (plus padding), so a
    // single-token column — a parameter name, a type — keeps its natural width
    // and never wraps mid-word. Only columns with internal break points (a list
    // of values) shrink below natural and wrap to make room.
    let mut min_w: Vec<f32> = headers
        .iter()
        .map(|h| longest_word_w(ctx.measure, h, true) + cell_pad * 2.0)
        .collect();
    for row in rows {
        for (ci, cell) in row.iter().enumerate().take(col_count) {
            let lw = longest_word_w(ctx.measure, cell, false) + cell_pad * 2.0;
            if lw > min_w[ci] {
                min_w[ci] = lw;
            }
        }
    }
    for ci in 0..col_count {
        min_w[ci] = min_w[ci].min(natural[ci]);
    }
    let col_widths = distribute_col_widths(&natural, &min_w, w);

    // ── Col x offsets ──────────────────────────────────────────────────────
    let col_x: Vec<f32> = {
        let mut acc = x;
        col_widths
            .iter()
            .map(|cw| {
                let cx = acc;
                acc += cw;
                cx
            })
            .collect()
    };

    // Returns the text x for a cell given its column left edge, available
    // width, measured text width, and alignment.
    let aligned_x = |col_left: f32, avail: f32, text_w: f32, a: ColAlign| -> f32 {
        match a {
            ColAlign::Right => (col_left + avail - text_w).max(col_left),
            ColAlign::Center => col_left + ((avail - text_w) / 2.0).max(0.0),
            _ => col_left,
        }
    };

    // ── Header row ─────────────────────────────────────────────────────────
    let header_h = line_h + cell_pad * 2.0;
    ctx.push(DrawCmd::FillRect {
        x,
        y: ctx.y,
        w,
        h: header_h,
        color: theme::SIDEBAR_BG,
    });
    for (i, header) in headers.iter().enumerate() {
        let avail = (col_widths[i] - cell_pad * 2.0).max(1.0);
        let col_left = col_x[i] + cell_pad;
        let a = alignments.get(i).copied().unwrap_or(ColAlign::None);
        let tw = (ctx.measure)(header, theme::BODY_FONT, theme::BODY_SIZE, true, false).min(avail);
        let tx = aligned_x(col_left, avail, tw, a);
        ctx.text(
            header,
            tx,
            ctx.y + cell_pad,
            avail,
            theme::BODY_FONT,
            theme::BODY_SIZE,
            true,
            false,
            theme::TEXT_BRIGHT,
            false,
        );
    }
    ctx.push(DrawCmd::StrokeLine {
        x0: x,
        y0: ctx.y + header_h,
        x1: x + w,
        y1: ctx.y + header_h,
        color: theme::BORDER,
    });
    ctx.y += header_h;

    // ── Body rows ──────────────────────────────────────────────────────────
    let table_body_top = ctx.y;
    for (ri, row) in rows.iter().enumerate() {
        let row_h = row
            .iter()
            .enumerate()
            .take(col_count)
            .map(|(ci, cell)| {
                let text_w = (col_widths[ci] - cell_pad * 2.0).max(1.0);
                wrapped_rows(ctx.measure, cell, theme::BODY_FONT, theme::BODY_SIZE, false, text_w)
                    * line_h
                    + cell_pad * 2.0
            })
            .fold(line_h + cell_pad * 2.0, f32::max);

        let row_bg = if ri % 2 == 0 {
            theme::BG
        } else {
            theme::SIDEBAR_BG
        };
        ctx.push(DrawCmd::FillRect {
            x,
            y: ctx.y,
            w,
            h: row_h,
            color: row_bg,
        });
        for (ci, cell) in row.iter().enumerate().take(col_count) {
            let avail = (col_widths[ci] - cell_pad * 2.0).max(1.0);
            let col_left = col_x[ci] + cell_pad;
            let a = alignments.get(ci).copied().unwrap_or(ColAlign::None);
            let tw =
                (ctx.measure)(cell, theme::BODY_FONT, theme::BODY_SIZE, false, false).min(avail);
            let tx = aligned_x(col_left, avail, tw, a);
            ctx.text(
                cell,
                tx,
                ctx.y + cell_pad,
                avail,
                theme::BODY_FONT,
                theme::BODY_SIZE,
                false,
                false,
                theme::TEXT,
                false,
            );
        }
        ctx.push(DrawCmd::StrokeLine {
            x0: x,
            y0: ctx.y + row_h,
            x1: x + w,
            y1: ctx.y + row_h,
            color: theme::BORDER,
        });
        ctx.y += row_h;
    }

    // ── Vertical column dividers ───────────────────────────────────────────
    let table_top = table_body_top - header_h;
    for i in 1..col_count {
        ctx.push(DrawCmd::StrokeLine {
            x0: col_x[i],
            y0: table_top,
            x1: col_x[i],
            y1: ctx.y,
            color: theme::BORDER,
        });
    }
    ctx.push(DrawCmd::StrokeLine {
        x0: x,
        y0: table_top,
        x1: x,
        y1: ctx.y,
        color: theme::BORDER,
    });
    ctx.push(DrawCmd::StrokeLine {
        x0: x + w,
        y0: table_top,
        x1: x + w,
        y1: ctx.y,
        color: theme::BORDER,
    });
}

fn layout_toolbar(ctx: &mut Ctx, items: &[ToolbarItem]) {
    if items.is_empty() {
        return;
    }

    let x = ctx.x();
    let w = ctx.avail_w();
    let n = items.len() as f32;
    let item_w = w / n;

    // Heights / spacing
    let top_pad: f32 = 10.0;
    let icon_h: f32 = 44.0;
    let icon_gap: f32 = 5.0;
    let label_sz: f32 = theme::BODY_SIZE - 2.0;
    let bot_pad: f32 = 8.0;
    // Drop text labels when columns are too narrow to display them legibly.
    let show_labels = item_w >= 72.0;
    let total_h = top_pad
        + icon_h
        + bot_pad
        + if show_labels {
            icon_gap + label_sz * theme::LINE_EXTRA
        } else {
            0.0
        };

    // Strip background
    ctx.push(DrawCmd::FillRect {
        x,
        y: ctx.y,
        w,
        h: total_h,
        color: theme::SIDEBAR_BG,
    });

    for (i, item) in items.iter().enumerate() {
        let ix = x + i as f32 * item_w;

        // Vertical divider (right edge of each item except last)
        if i + 1 < items.len() {
            ctx.push(DrawCmd::StrokeLine {
                x0: ix + item_w,
                y0: ctx.y,
                x1: ix + item_w,
                y1: ctx.y + total_h,
                color: theme::BORDER,
            });
        }

        // Icon: centred horizontally, padded from top
        let icon_disp = icon_h.min(item_w - 16.0); // keep icon within column
        let icon_x = ix + (item_w - icon_disp) / 2.0;
        let icon_y = ctx.y + top_pad;
        ctx.push(DrawCmd::Image {
            x: icon_x,
            y: icon_y,
            w: icon_disp,
            h: icon_disp,
            path: item.image_path.clone(),
        });

        // Label: centred below icon (only when wide enough)
        if show_labels {
            let label_y = icon_y + icon_h + icon_gap;
            let label_x = ix + 4.0;
            let label_w = item_w - 8.0;
            ctx.text(
                &item.label,
                label_x,
                label_y,
                label_w,
                theme::BODY_FONT,
                label_sz,
                false,
                false,
                theme::TEXT,
                false,
            );
        }

        // Hit region for the whole cell
        ctx.toolbar_hits.push(HitRegion {
            x0: ix,
            y0: ctx.y,
            x1: ix + item_w,
            y1: ctx.y + total_h,
            href: item.href.clone(),
        });
    }

    // Top and bottom border lines
    ctx.push(DrawCmd::StrokeLine {
        x0: x,
        y0: ctx.y,
        x1: x + w,
        y1: ctx.y,
        color: theme::BORDER,
    });
    ctx.push(DrawCmd::StrokeLine {
        x0: x,
        y0: ctx.y + total_h,
        x1: x + w,
        y1: ctx.y + total_h,
        color: theme::BORDER,
    });

    ctx.y += total_h;
}

fn layout_rule(ctx: &mut Ctx) {
    ctx.y += 8.0;
    let x = ctx.x();
    let w = ctx.avail_w();
    ctx.push(DrawCmd::FillRect {
        x,
        y: ctx.y,
        w,
        h: theme::H_RULE_H,
        color: theme::RULE,
    });
    ctx.y += theme::H_RULE_H + 8.0;
}

fn collect_inlines_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text(t)
            | Inline::Bold(t)
            | Inline::Italic(t)
            | Inline::BoldItalic(t)
            | Inline::Code(t) => s.push_str(t),
            Inline::Link { text, .. } => s.push_str(text),
            Inline::Image { alt, .. } => s.push_str(alt),
            Inline::SoftBreak | Inline::HardBreak => s.push(' '),
        }
    }
    s
}

// Rough approximation of wrapped line count for height pre-calculation.
/// Width of the widest whitespace-separated word in `text` at the body size —
/// the floor below which a table column would have to wrap mid-word.
fn longest_word_w(measure: fn(&str, &str, f32, bool, bool) -> f32, text: &str, bold: bool) -> f32 {
    text.split_whitespace()
        .map(|wd| measure(wd, theme::BODY_FONT, theme::BODY_SIZE, bold, false))
        .fold(0.0_f32, f32::max)
}

/// Spread table width `w` across columns. `natural[i]` is each column's
/// max-content width and `min_w[i]` its widest unbreakable word. Columns that
/// can't wrap (a name, a type) keep their natural width; the shrink lands on the
/// flexible columns (value lists), which then wrap.
fn distribute_col_widths(natural: &[f32], min_w: &[f32], w: f32) -> Vec<f32> {
    let n = natural.len();
    let total_natural: f32 = natural.iter().sum::<f32>().max(1.0);
    if total_natural <= w {
        // Fits: keep natural widths, give the leftover to the widest column so
        // the table fills the pane instead of leaving a ragged right edge.
        let mut cw = natural.to_vec();
        if let Some(widest) = (0..n).max_by(|&a, &b| {
            natural[a].partial_cmp(&natural[b]).unwrap_or(std::cmp::Ordering::Equal)
        }) {
            cw[widest] += w - total_natural;
        }
        return cw;
    }
    let total_min: f32 = min_w.iter().sum::<f32>().max(1.0);
    if total_min >= w {
        // Even the minimums overflow — best effort, proportional to the minimums.
        return min_w.iter().map(|m| (m / total_min) * w).collect();
    }
    // Give each column its minimum, then hand out the remaining width in
    // proportion to each column's wrappable slack (natural - min).
    let extra = w - total_min;
    let total_slack: f32 =
        natural.iter().zip(min_w).map(|(nat, m)| (nat - m).max(0.0)).sum::<f32>().max(1.0);
    natural
        .iter()
        .zip(min_w)
        .map(|(nat, m)| m + (nat - m).max(0.0) / total_slack * extra)
        .collect()
}

/// Accurate wrapped-row count: greedy word-wrap on real measured widths, the way
/// the renderer (DirectWrite word-wrap) actually breaks. A character-count guess
/// undercounts for wide glyphs/monospace, which makes the reserved height fall
/// short and the next line overprint ("overtype") — measuring fixes that.
fn wrapped_rows(
    measure: fn(&str, &str, f32, bool, bool) -> f32,
    text: &str,
    font: &'static str,
    size: f32,
    bold: bool,
    max_w: f32,
) -> f32 {
    if text.trim().is_empty() {
        return 1.0;
    }
    let max_w = max_w.max(1.0);
    let mut rows = 1.0_f32;
    let mut x = 0.0_f32;
    // Walk (whitespace-run, word) pairs, measuring the *literal* spacing — code
    // lines use multi-space alignment padding that the renderer keeps, so it must
    // count toward the line width.
    let mut rest = text;
    while !rest.is_empty() {
        let ws_end = rest.find(|c: char| !c.is_whitespace()).unwrap_or(rest.len());
        let ws_w = if ws_end == 0 { 0.0 } else { measure(&rest[..ws_end], font, size, bold, false) };
        rest = &rest[ws_end..];
        if rest.is_empty() {
            break;
        }
        let w_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word_w = measure(&rest[..w_end], font, size, bold, false);
        rest = &rest[w_end..];
        if x > 0.0 && x + ws_w + word_w > max_w {
            rows += 1.0; // wrap: the run-leading whitespace is dropped at the break
            x = word_w;
        } else {
            x += ws_w + word_w;
        }
        // A single word wider than the line still wraps (character break).
        while x > max_w {
            rows += 1.0;
            x -= max_w;
        }
    }
    rows
}

#[cfg(test)]
mod table_width_tests {
    use super::distribute_col_widths;

    #[test]
    fn wide_value_column_does_not_starve_name_or_type() {
        // # | parameter | type | values, where values is a huge constant list
        // that wraps down to a single constant (~95px).
        let natural = [30.0, 90.0, 120.0, 800.0];
        let min_w = [30.0, 90.0, 120.0, 95.0];
        let cw = distribute_col_widths(&natural, &min_w, 600.0);
        assert!((cw[1] - 90.0).abs() < 0.5, "name keeps natural width: {cw:?}");
        assert!((cw[2] - 120.0).abs() < 0.5, "type keeps natural width: {cw:?}");
        assert!(cw[3] > 95.0 && cw[3] < 800.0, "values shrinks to wrap: {cw:?}");
        assert!((cw.iter().sum::<f32>() - 600.0).abs() < 0.5, "fills the pane: {cw:?}");
    }

    #[test]
    fn fits_naturally_then_fills_pane() {
        let natural = [30.0, 90.0, 120.0, 100.0]; // sum 340 < 600
        let min_w = [30.0, 90.0, 120.0, 60.0];
        let cw = distribute_col_widths(&natural, &min_w, 600.0);
        assert!(cw[1] >= 90.0 && cw[2] >= 120.0, "narrow cols keep >= natural: {cw:?}");
        assert!((cw.iter().sum::<f32>() - 600.0).abs() < 0.5, "fills the pane: {cw:?}");
    }

    #[test]
    fn minimums_overflow_falls_back_proportionally() {
        // Two unbreakable columns whose minimums already exceed the width.
        let natural = [400.0, 400.0];
        let min_w = [400.0, 400.0];
        let cw = distribute_col_widths(&natural, &min_w, 600.0);
        assert!((cw.iter().sum::<f32>() - 600.0).abs() < 0.5, "still bounded to width: {cw:?}");
    }
}
