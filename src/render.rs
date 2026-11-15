//! Draws the terminal on an iced canvas. Two modes:
//! - blocks: one card per command (header, command, output), newest at the bottom;
//! - plain: the raw grid, for full-screen apps and shells without block marks.
//! Mouse drags select text, the wheel scrolls history. Hovering a finished block shows its buttons.

use crate::term::{Block, Cell, Color as TColor, MouseMode, Row, Style, Term};
use crate::Msg;
use iced::advanced::text::{self as atext, Paragraph as _};
use iced::alignment::Vertical;
use iced::font::{Style as FontStyle, Weight};
use iced::keyboard;
use iced::mouse::{self, ScrollDelta};
use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Path, Program, Stroke, Text};
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme};
use std::time::Duration;

pub const FONT_SIZE: f32 = 14.0;
const LINE: f32 = 1.25;
const PAD: f32 = 6.0; // plain mode inset
const LEFT: f32 = 18.0; // block text inset
const RIGHT: f32 = 16.0;
const PAD_TOP: f32 = 10.0;
const HEADER_H: f32 = 20.0;
const PAD_BOTTOM: f32 = 12.0;
const SMALL: f32 = 12.0;
const GAP: f32 = 14.0; // between header buttons
pub const FG: Color = Color::from_rgb(0.87, 0.87, 0.89);
pub const BG: Color = Color::from_rgb(0.10, 0.10, 0.12);
pub const MUTED: Color = Color::from_rgb(0.52, 0.53, 0.58);
pub const ACCENT: Color = Color::from_rgb(0.45, 0.62, 1.0);
const DIVIDER: Color = Color::from_rgb(0.17, 0.17, 0.20);
const RED: Color = Color::from_rgb(0.94, 0.38, 0.42);
pub const SELECTION: Color = Color::from_rgba(0.3, 0.55, 1.0, 0.35);

pub type Pos = (usize, usize); // (absolute line, column)

pub fn font() -> Font {
    Font::with_name("Menlo")
}

/// Width and height of one character cell, measured from the real font so text runs line up.
pub fn cell_size() -> Size {
    let p = <Renderer as atext::Renderer>::Paragraph::with_text(atext::Text {
        content: "MMMMMMMMMM",
        bounds: Size::INFINITE,
        size: Pixels(FONT_SIZE),
        line_height: LineHeight::Relative(LINE),
        font: font(),
        align_x: atext::Alignment::Left,
        align_y: Vertical::Top,
        shaping: Shaping::Basic,
        wrapping: Wrapping::None,
    });
    Size::new(p.min_width() / 10.0, FONT_SIZE * LINE)
}

fn blocks_mode(term: &Term) -> bool {
    term.integrated && !term.alt_screen
}

/// Grid size that fits `area`. In block mode the running block's header and command row take room.
pub fn grid_size(term: &Term, area: Size, cell: Size) -> (usize, usize) {
    let (w, h) = if blocks_mode(term) {
        (area.width - LEFT - RIGHT, area.height - chrome(cell, true))
    } else {
        (area.width - 2.0 * PAD, area.height - 2.0 * PAD)
    };
    ((w / cell.width).floor().max(2.0) as usize, (h / cell.height).floor().max(2.0) as usize)
}

/// Height of a block besides its output lines.
fn chrome(cell: Size, has_cmd: bool) -> f32 {
    PAD_TOP + HEADER_H + if has_cmd { cell.height } else { 0.0 } + PAD_BOTTOM
}

fn blank(r: &Row) -> bool {
    r.iter().all(|c| c.ch == ' ' && c.style.bg == TColor::Default)
}

/// Output lines of a block as [start, end). A running block reaches down to the cursor.
fn output_range(term: &Term, b: &Block) -> (usize, usize) {
    let Some(start) = b.start else { return (0, 0) };
    let start = start.max(term.first_line());
    let mut end = b.end.unwrap_or_else(|| {
        let last = (0..term.rows).rev().find(|&r| !blank(&term.screen()[r])).unwrap_or(0);
        term.abs(term.row).max(term.abs(last)) + 1
    });
    while end > start && term.line(end - 1).is_none_or(blank) {
        end -= 1;
    }
    if b.running() && term.cursor_visible {
        end = end.max(term.abs(term.row) + 1); // keep the line the program is typing on
    }
    (start, end.max(start))
}

/// Buttons on a finished block's header, shown while the mouse is over the block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockAct {
    CopyOutput,
    CopyCommand,
    Rerun,
}

impl BlockAct {
    fn label(self) -> &'static str {
        match self {
            BlockAct::CopyOutput => "copy output",
            BlockAct::CopyCommand => "copy command",
            BlockAct::Rerun => "rerun",
        }
    }
}

/// What a block offers: nothing while it runs, and rerun only when the shell is at its prompt.
pub fn block_acts(term: &Term, b: &Block, idle: bool) -> Vec<BlockAct> {
    if b.running() {
        return vec![];
    }
    let (start, end) = output_range(term, b);
    let mut acts = Vec::new();
    if end > start {
        acts.push(BlockAct::CopyOutput);
    }
    if !b.cmd.is_empty() {
        acts.push(BlockAct::CopyCommand);
        if idle {
            acts.push(BlockAct::Rerun);
        }
    }
    acts
}

/// A block's output as plain text, one line per row, trailing spaces trimmed.
pub fn block_output(term: &Term, b: &Block) -> String {
    let (start, end) = output_range(term, b);
    if end == start {
        return String::new();
    }
    selected_text(term, ((start, 0), (end - 1, term.cols)))
}

fn small_width(s: &str) -> f32 {
    <Renderer as atext::Renderer>::Paragraph::with_text(atext::Text {
        content: s,
        bounds: Size::INFINITE,
        size: Pixels(SMALL),
        line_height: LineHeight::default(),
        font: Font::DEFAULT,
        align_x: atext::Alignment::Left,
        align_y: Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
    })
    .min_width()
}

struct Placed {
    i: usize,
    y: f32,
    h: f32,
    out: (usize, usize),
    out_y: f32,
}

/// Lays blocks out top-down, pinned to the bottom once they overflow; `scroll` (px) looks back.
fn place(term: &Term, cell: Size, height: f32, scroll: f32) -> (Vec<Placed>, f32) {
    let mut all = Vec::with_capacity(term.blocks.len());
    let mut total = 0.0;
    for (i, b) in term.blocks.iter().enumerate() {
        let out = output_range(term, b);
        let h = chrome(cell, !b.cmd.is_empty()) + (out.1 - out.0) as f32 * cell.height;
        all.push((i, total, h, out));
        total += h;
    }
    let y0 = if total <= height { 0.0 } else { height - total + scroll.min(total - height) };
    let placed = all
        .into_iter()
        .map(|(i, y, h, out)| {
            let y = y0 + y;
            let cmd_h = if term.blocks[i].cmd.is_empty() { 0.0 } else { cell.height };
            Placed { i, y, h, out, out_y: y + PAD_TOP + HEADER_H + cmd_h }
        })
        .filter(|p| p.y + p.h > 0.0 && p.y < height)
        .collect();
    (placed, total)
}

/// Total height of all blocks, so the app can clamp scrolling.
pub fn content_height(term: &Term, cell: Size) -> f32 {
    place(term, cell, f32::MAX, 0.0).1
}

fn ordered((a, b): (Pos, Pos)) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

pub fn selected_text(term: &Term, sel: (Pos, Pos)) -> String {
    let ((l0, c0), (l1, c1)) = ordered(sel);
    let mut out = Vec::new();
    for l in l0..=l1 {
        let Some(row) = term.line(l) else { continue };
        let from = if l == l0 { c0 } else { 0 };
        let to = if l == l1 { (c1 + 1).min(row.len()) } else { row.len() };
        let mut s = String::new();
        row[from.min(to)..to].iter().filter(|c| !c.wide_tail).for_each(|c| c.push_to(&mut s));
        out.push(s.trim_end().to_string());
    }
    out.join("\n")
}

fn ansi(i: u8) -> Color {
    const BASE: [(u8, u8, u8); 16] = [
        (0x1c, 0x1c, 0x1e), (0xe0, 0x6c, 0x75), (0x98, 0xc3, 0x79), (0xe5, 0xc0, 0x7b),
        (0x61, 0xaf, 0xef), (0xc6, 0x78, 0xdd), (0x56, 0xb6, 0xc2), (0xc8, 0xcc, 0xd4),
        (0x5c, 0x63, 0x70), (0xff, 0x7b, 0x86), (0xb5, 0xe8, 0x90), (0xff, 0xd6, 0x8a),
        (0x7d, 0xc4, 0xff), (0xde, 0x9b, 0xf5), (0x7f, 0xd8, 0xe3), (0xff, 0xff, 0xff),
    ];
    let (r, g, b) = match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let n = i - 16;
            (lv(n / 36), lv(n / 6 % 6), lv(n % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    };
    Color::from_rgb8(r, g, b)
}

fn colors(s: &Style) -> (Color, Color) {
    let pick = |c: TColor, default: Color| match c {
        TColor::Default => default,
        TColor::Idx(i) => ansi(i),
        TColor::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    };
    let (mut fg, mut bg) = (pick(s.fg, FG), pick(s.bg, BG));
    if s.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if s.dim {
        fg.a = 0.6;
    }
    (fg, bg)
}

fn styled_font(bold: bool, italic: bool) -> Font {
    Font {
        weight: if bold { Weight::Bold } else { Weight::Normal },
        style: if italic { FontStyle::Italic } else { FontStyle::Normal },
        ..font()
    }
}

fn label(f: &mut Frame, s: String, at: Point, color: Color, right: bool) {
    f.fill_text(Text {
        content: s,
        position: at,
        color,
        size: Pixels(SMALL),
        font: Font::DEFAULT,
        align_x: if right { atext::Alignment::Right } else { atext::Alignment::Left },
        shaping: Shaping::Advanced,
        ..Text::default()
    });
}

pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

fn took(d: Duration) -> String {
    let s = d.as_secs_f32();
    if s < 60.0 { format!("{s:.1}s") } else { format!("{}m {:02}s", s as u64 / 60, s as u64 % 60) }
}

/// Block characters (U+2580-259F) as (opacity, rects in cell fractions x0,y0,x1,y1). Drawn as shapes, not
/// font glyphs, so neighbours tile with no gaps (Claude's logo, progress bars).
fn block_shape(ch: char) -> Option<(f32, Vec<[f32; 4]>)> {
    const UL: [f32; 4] = [0.0, 0.0, 0.5, 0.5];
    const UR: [f32; 4] = [0.5, 0.0, 1.0, 0.5];
    const LL: [f32; 4] = [0.0, 0.5, 0.5, 1.0];
    const LR: [f32; 4] = [0.5, 0.5, 1.0, 1.0];
    let n = ch as u32;
    let eighth = |k: u32| k as f32 / 8.0;
    let rects = match n {
        0x2580 => vec![[0.0, 0.0, 1.0, 0.5]],
        0x2581..=0x2588 => vec![[0.0, 1.0 - eighth(n - 0x2580), 1.0, 1.0]], // lower 1/8 .. full
        0x2589..=0x258F => vec![[0.0, 0.0, eighth(0x2590 - n), 1.0]],      // left 7/8 .. 1/8
        0x2590 => vec![[0.5, 0.0, 1.0, 1.0]],
        0x2591..=0x2593 => return Some(((n - 0x2590) as f32 * 0.25, vec![[0.0, 0.0, 1.0, 1.0]])), // shades
        0x2594 => vec![[0.0, 0.0, 1.0, 0.125]],
        0x2595 => vec![[0.875, 0.0, 1.0, 1.0]],
        0x2596 => vec![LL],
        0x2597 => vec![LR],
        0x2598 => vec![UL],
        0x2599 => vec![UL, LL, LR],
        0x259A => vec![UL, LR],
        0x259B => vec![UL, UR, LL],
        0x259C => vec![UL, UR, LR],
        0x259D => vec![UR],
        0x259E => vec![UR, LL],
        0x259F => vec![UR, LL, LR],
        _ => return None,
    };
    Some((1.0, rects))
}

/// One terminal row: backgrounds, selection, then text. Plain-ASCII runs with one style share a
/// draw call; anything else is drawn alone at its cell so fallback-font glyphs can't shift columns.
fn paint_row(f: &mut Frame, row: &Row, cols: usize, x0: f32, y: f32, cell: Size, sel: Option<(usize, usize)>) {
    let (cw, ch) = (cell.width, cell.height);
    let x_of = |c: usize| x0 + c as f32 * cw;
    let cells = &row[..row.len().min(cols)];
    for (c, cell) in cells.iter().enumerate() {
        let (_, bg) = colors(&cell.style);
        if bg != BG {
            f.fill_rectangle(Point::new(x_of(c), y), Size::new(cw, ch), bg);
        }
    }
    if let Some((from, to)) = sel {
        f.fill_rectangle(Point::new(x_of(from), y), Size::new(to.saturating_sub(from) as f32 * cw, ch), SELECTION);
    }
    let mut c = 0;
    while c < cells.len() {
        let cell = &cells[c];
        if cell.wide_tail || (cell.ch == ' ' && !cell.style.underline) {
            c += 1;
            continue;
        }
        let (fg, _) = colors(&cell.style);
        if let Some((alpha, rects)) = block_shape(cell.ch) {
            for [x0, y0, x1, y1] in rects {
                // Reach half a pixel into the next cell on shared edges so anti-aliasing leaves no seam.
                let (r, b) = (if x1 == 1.0 { 0.5 } else { 0.0 }, if y1 == 1.0 { 0.5 } else { 0.0 });
                let at = Point::new(x_of(c) + x0 * cw, y + y0 * ch);
                f.fill_rectangle(at, Size::new((x1 - x0) * cw + r, (y1 - y0) * ch + b), Color { a: fg.a * alpha, ..fg });
            }
            c += 1;
            continue;
        }
        let start = c;
        let mut content = String::new();
        let plain = |x: &Cell| x.ch.is_ascii() && !x.has_marks() && !x.wide_tail;
        if plain(cell) {
            while c < cells.len() && plain(&cells[c]) && cells[c].style == cell.style {
                content.push(cells[c].ch);
                c += 1;
            }
        } else {
            cell.push_to(&mut content);
            c += 1;
        }
        f.fill_text(Text {
            content,
            position: Point::new(x_of(start), y),
            color: fg,
            size: Pixels(FONT_SIZE),
            line_height: LineHeight::Relative(LINE),
            font: styled_font(cell.style.bold, cell.style.italic),
            shaping: if plain(cell) { Shaping::Basic } else { Shaping::Advanced },
            ..Text::default()
        });
        if cell.style.underline {
            f.fill_rectangle(Point::new(x_of(start), y + ch - 2.0), Size::new((c - start) as f32 * cw, 1.0), fg);
        }
    }
}

/// Solid when your keys go straight to the program, hollow when they go to the input box.
fn cursor(f: &mut Frame, x: f32, y: f32, cell: Size, solid: bool) {
    if solid {
        f.fill_rectangle(Point::new(x, y), cell, Color { a: 0.6, ..ACCENT });
    } else {
        f.stroke(&Path::rectangle(Point::new(x, y), cell), Stroke::default().with_color(FG).with_width(1.0));
    }
}

pub struct View<'a> {
    pub term: &'a Term,
    pub scroll: f32, // px looked back into history
    pub sel: Option<(Pos, Pos)>,
    pub cell: Size,
    pub cache: &'a canvas::Cache,
    pub direct: bool, // typing goes to the running program, not the input box
    pub hover: Option<usize>, // block under the mouse; its buttons show
    pub done: Option<(usize, BlockAct)>, // button just clicked, shown as "copied" until the mouse leaves
    pub idle: bool, // the shell is at its prompt, so a command can be rerun
}

impl View<'_> {
    fn sel_cols(&self, abs: usize) -> Option<(usize, usize)> {
        let ((l0, c0), (l1, c1)) = ordered(self.sel?);
        (l0..=l1).contains(&abs).then(|| (if abs == l0 { c0 } else { 0 }, if abs == l1 { c1 + 1 } else { self.term.cols }))
    }

    fn plain_top(&self) -> usize {
        let back = (self.scroll / self.cell.height) as usize;
        self.term.abs(0).saturating_sub(back).max(self.term.first_line())
    }

    fn hit(&self, p: Point, size: Size) -> Option<Pos> {
        let col = |x0: f32| (((p.x - x0) / self.cell.width).max(0.0) as usize).min(self.term.cols - 1);
        if !blocks_mode(self.term) {
            let row = (((p.y - PAD) / self.cell.height).max(0.0) as usize).min(self.term.rows - 1);
            return Some((self.plain_top() + row, col(PAD)));
        }
        let (placed, _) = place(self.term, self.cell, size.height, self.scroll);
        placed.iter().find_map(|b| {
            let k = ((p.y - b.out_y) / self.cell.height).floor();
            (k >= 0.0 && (k as usize) < b.out.1 - b.out.0).then(|| (b.out.0 + k as usize, col(LEFT)))
        })
    }

    /// Index of the block under `p` (canvas coordinates), in block mode.
    fn block_at(&self, p: Point, size: Size) -> Option<usize> {
        if !blocks_mode(self.term) {
            return None;
        }
        let (placed, _) = place(self.term, self.cell, size.height, self.scroll);
        placed.iter().find(|b| p.y >= b.y && p.y < b.y + b.h).map(|b| b.i)
    }

    /// The hovered block's buttons, right-aligned on its header row.
    fn act_rects(&self, p: &Placed, width: f32) -> Vec<(BlockAct, Rectangle)> {
        let mut x = width - RIGHT;
        let mut rects: Vec<_> = block_acts(self.term, &self.term.blocks[p.i], self.idle)
            .into_iter()
            .rev()
            .map(|a| {
                let w = small_width(a.label());
                x -= w;
                let r = Rectangle::new(Point::new(x, p.y + PAD_TOP - 3.0), Size::new(w, HEADER_H));
                x -= GAP;
                (a, r)
            })
            .collect();
        rects.reverse();
        rects
    }

    /// The button under `p`, if the mouse is over a block that shows them.
    fn act_at(&self, p: Point, size: Size) -> Option<(usize, BlockAct)> {
        let i = self.hover?;
        let (placed, _) = place(self.term, self.cell, size.height, self.scroll);
        let b = placed.iter().find(|b| b.i == i)?;
        self.act_rects(b, size.width).into_iter().find(|(_, r)| r.contains(p)).map(|(a, _)| (i, a))
    }

    fn paint_plain(&self, f: &mut Frame) {
        let (t, cell) = (self.term, self.cell);
        let top = self.plain_top();
        for vr in 0..t.rows {
            if let Some(row) = t.line(top + vr) {
                paint_row(f, row, t.cols, PAD, PAD + vr as f32 * cell.height, cell, self.sel_cols(top + vr));
            }
        }
        if top == t.abs(0) && t.cursor_visible {
            cursor(f, PAD + t.col as f32 * cell.width, PAD + t.row as f32 * cell.height, cell, self.direct);
        }
    }

    fn paint_blocks(&self, f: &mut Frame) {
        let (t, cell) = (self.term, self.cell);
        let size = f.size();
        if t.blocks.is_empty() {
            label(f, "Type a command below. Each one gets its own block here.".into(), Point::new(LEFT, PAD_TOP), MUTED, false);
            return;
        }
        let (placed, _) = place(t, cell, size.height, self.scroll);
        for p in &placed {
            let b = &t.blocks[p.i];
            if p.i > 0 {
                f.fill_rectangle(Point::new(0.0, p.y), Size::new(size.width, 1.0), DIVIDER);
            }
            let failed = b.exit.is_some_and(|c| c != 0);
            let edge = if b.running() { Some(ACCENT) } else if failed { Some(RED) } else { None };
            if let Some(c) = edge {
                f.fill_rectangle(Point::new(0.0, p.y), Size::new(3.0, p.h), c);
            }
            // Header: where it ran on the left, how it went on the right.
            let mut place_label = tilde(&b.cwd);
            if !b.branch.is_empty() {
                place_label = format!("{place_label}   {}", b.branch);
            }
            if b.cmd.is_empty() && p.i == 0 && !b.running() {
                place_label = format!("{place_label}   shell startup");
            }
            label(f, place_label, Point::new(LEFT, p.y + PAD_TOP), MUTED, false);
            let acts = if self.hover == Some(p.i) { self.act_rects(p, size.width) } else { vec![] };
            for (a, r) in &acts {
                let copied = self.done == Some((p.i, *a)) && *a != BlockAct::Rerun;
                let (s, c) = if copied { ("copied", ACCENT) } else { (a.label(), MUTED) };
                label(f, s.into(), Point::new(r.x, p.y + PAD_TOP), c, false);
            }
            let status_x = acts.first().map_or(size.width - RIGHT, |(_, r)| r.x - GAP);
            let (status, color) = match (b.running(), b.exit, b.took) {
                (true, ..) if b.began.elapsed() >= Duration::from_secs(1) => (format!("running  ·  {}", took(b.began.elapsed())), ACCENT),
                (true, ..) => ("running".to_string(), ACCENT),
                (false, Some(c), Some(d)) if c != 0 && d >= Duration::from_millis(100) => (format!("exit {c}  ·  {}", took(d)), RED),
                (false, Some(c), _) if c != 0 => (format!("exit {c}"), RED),
                (false, _, Some(d)) if d >= Duration::from_millis(100) => (took(d), MUTED), // instant ones say nothing
                _ => (String::new(), MUTED),
            };
            label(f, status, Point::new(status_x, p.y + PAD_TOP), color, true);
            if !b.cmd.is_empty() {
                let first = b.cmd.lines().next().unwrap_or_default();
                let more = if b.cmd.lines().count() > 1 { "  …" } else { "" };
                f.fill_text(Text {
                    content: format!("{first}{more}"),
                    position: Point::new(LEFT, p.y + PAD_TOP + HEADER_H),
                    color: FG,
                    size: Pixels(FONT_SIZE),
                    line_height: LineHeight::Relative(LINE),
                    font: styled_font(true, false),
                    shaping: Shaping::Advanced,
                    ..Text::default()
                });
            }
            for abs in p.out.0..p.out.1 {
                let y = p.out_y + (abs - p.out.0) as f32 * cell.height;
                if y + cell.height < 0.0 || y > size.height {
                    continue;
                }
                if let Some(row) = t.line(abs) {
                    paint_row(f, row, t.cols, LEFT, y, cell, self.sel_cols(abs));
                }
            }
            let cur = t.abs(t.row);
            if b.running() && t.cursor_visible && (p.out.0..p.out.1).contains(&cur) {
                let y = p.out_y + (cur - p.out.0) as f32 * cell.height;
                cursor(f, LEFT + t.col as f32 * cell.width, y, cell, self.direct);
            }
        }
    }
}

#[derive(Default)]
pub struct Drag {
    selecting: bool,
    held: Option<u8>,             // button down in a program that gets mouse reports
    last: Option<(usize, usize)>, // cell of the last report, so motion inside one cell sends nothing
    wheel: f32,                   // trackpad pixels not yet worth a wheel step
    mods: keyboard::Modifiers,
}

impl View<'_> {
    /// Mouse events for a full-screen program that asked for them (vim with mouse=a, htop, tmux), as
    /// xterm reports them. Shift keeps the mouse for Nib, so text can still be selected.
    fn mouse_report(&self, event: &Event, at: Option<Point>, st: &mut Drag) -> Option<Vec<u8>> {
        let t = self.term;
        if t.mouse == MouseMode::Off || blocks_mode(t) || st.mods.shift() || st.selecting {
            return None;
        }
        let cell = |p: Point| {
            let col = (((p.x - PAD) / self.cell.width).max(0.0) as usize).min(t.cols - 1);
            let row = (((p.y - PAD) / self.cell.height).max(0.0) as usize).min(t.rows - 1);
            (col, row)
        };
        let mods = (false, st.mods.alt(), st.mods.control());
        let report = |b: u8, motion: bool, release: bool, (c, r): (usize, usize)| {
            crate::keys::mouse_report(b, motion, release, c, r, mods, t.mouse_sgr)
        };
        let button = |b: &mouse::Button| match b {
            mouse::Button::Left => Some(0),
            mouse::Button::Middle => Some(1),
            mouse::Button::Right => Some(2),
            _ => None,
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(b)) => {
                let (b, at) = (button(b)?, cell(at?));
                (st.held, st.last) = (Some(b), Some(at));
                report(b, false, false, at)
            }
            Event::Mouse(mouse::Event::ButtonReleased(b)) => {
                let b = button(b)?;
                st.held.take()?; // only a release for a press the program saw
                report(b, false, true, at.map(cell).or(st.last)?)
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let at = cell(at?);
                let b = match (st.held, t.mouse) {
                    (Some(b), MouseMode::Drag | MouseMode::Motion) => b,
                    (None, MouseMode::Motion) => 3,
                    _ => return None,
                };
                if st.last == Some(at) {
                    return None;
                }
                st.last = Some(at);
                report(b, true, false, at)
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let at = cell(at?);
                let steps = match delta {
                    ScrollDelta::Lines { y, .. } => y.round() as i32,
                    ScrollDelta::Pixels { y, .. } => {
                        st.wheel += y;
                        let n = (st.wheel / self.cell.height).trunc();
                        st.wheel -= n * self.cell.height;
                        n as i32
                    }
                };
                let one = report(if steps > 0 { 64 } else { 65 }, false, false, at)?;
                Some(one.repeat(steps.unsigned_abs() as usize)) // empty still keeps the wheel from Nib
            }
            _ => None,
        }
    }
}

impl Program<Msg> for View<'_> {
    type State = Drag;

    fn update(&self, drag: &mut Drag, event: &Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<Action<Msg>> {
        let local = || cursor.position().map(|p| Point::new(p.x - bounds.x, p.y - bounds.y));
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            drag.mods = *m;
            return None;
        }
        let inside = cursor.position_in(bounds).and_then(|_| local());
        let release = matches!(event, Event::Mouse(mouse::Event::ButtonReleased(_)));
        if let Some(bytes) = self.mouse_report(event, if release { local() } else { inside }, drag) {
            return Some(Action::publish(Msg::Write(bytes)).and_capture());
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                cursor.position_in(bounds)?;
                if let Some((i, a)) = self.act_at(local()?, bounds.size()) {
                    return Some(Action::publish(Msg::Block(i, a)).and_capture());
                }
                let pos = self.hit(local()?, bounds.size());
                drag.selecting = pos.is_some();
                Some(Action::publish(pos.map_or(Msg::SelClear, Msg::SelStart)))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if drag.selecting => {
                Some(Action::publish(Msg::SelTo(self.hit(local()?, bounds.size())?)))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let over = cursor.position_in(bounds).and_then(|_| self.block_at(local()?, bounds.size()));
                (over != self.hover).then(|| Action::publish(Msg::Hover(over)))
            }
            Event::Mouse(mouse::Event::CursorLeft) => self.hover.is_some().then(|| Action::publish(Msg::Hover(None))),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if drag.selecting => {
                drag.selecting = false;
                Some(Action::publish(Msg::Release))
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let px = match delta {
                    ScrollDelta::Lines { y, .. } => y * 3.0 * self.cell.height,
                    ScrollDelta::Pixels { y, .. } => *y,
                };
                Some(Action::publish(Msg::Scroll(px)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(&self, _: &Drag, renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        vec![self.cache.draw(renderer, bounds.size(), |f| {
            f.fill_rectangle(Point::ORIGIN, f.size(), BG);
            if blocks_mode(self.term) { self.paint_blocks(f) } else { self.paint_plain(f) }
        })]
    }

    fn mouse_interaction(&self, _: &Drag, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        match cursor.position_in(bounds) {
            Some(p) if self.act_at(p, bounds.size()).is_some() => mouse::Interaction::Pointer,
            Some(_) => mouse::Interaction::Text,
            None => mouse::Interaction::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{block_acts, block_output, block_shape, BlockAct, Drag, View, PAD};
    use crate::term::Term;
    use iced::mouse::{self, Button, ScrollDelta};
    use iced::widget::canvas::{Cache, Event};
    use iced::{keyboard, Point, Size};

    #[test]
    fn full_screen_programs_get_mouse_reports() {
        let mut t = Term::new(80, 24);
        t.feed(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h"); // like vim with mouse=a
        let cache = Cache::new();
        let cell = Size::new(10.0, 20.0);
        let v = View { term: &t, scroll: 0.0, sel: None, cell, cache: &cache, direct: false, hover: None, done: None, idle: false };
        let mut st = Drag::default();
        let at = |c: f32, r: f32| Some(Point::new(PAD + c * 10.0 + 1.0, PAD + r * 20.0 + 1.0));
        let moved = Event::Mouse(mouse::Event::CursorMoved { position: Point::ORIGIN });
        let send = |e: &Event, p, st: &mut Drag| v.mouse_report(e, p, st).map(|b| String::from_utf8(b).unwrap());
        assert_eq!(send(&Event::Mouse(mouse::Event::ButtonPressed(Button::Left)), at(4.0, 2.0), &mut st).as_deref(), Some("\x1b[<0;5;3M"));
        assert_eq!(send(&moved, at(5.0, 2.0), &mut st).as_deref(), Some("\x1b[<32;6;3M"), "drag to the next cell");
        assert_eq!(send(&moved, at(5.2, 2.0), &mut st), None, "same cell: nothing new");
        assert_eq!(send(&Event::Mouse(mouse::Event::ButtonReleased(Button::Left)), at(5.0, 2.0), &mut st).as_deref(), Some("\x1b[<0;6;3m"));
        assert_eq!(send(&moved, at(9.0, 9.0), &mut st), None, "no button held: mode 1002 sends no motion");
        let wheel = Event::Mouse(mouse::Event::WheelScrolled { delta: ScrollDelta::Pixels { x: 0.0, y: 45.0 } });
        assert_eq!(send(&wheel, at(0.0, 0.0), &mut st).as_deref(), Some("\x1b[<64;1;1M\x1b[<64;1;1M"), "45px is two steps of 20");
        assert_eq!(send(&wheel, at(0.0, 0.0), &mut st).as_deref(), Some("\x1b[<64;1;1M\x1b[<64;1;1M"), "the 5px left over carries");
        st.mods = keyboard::Modifiers::SHIFT;
        assert_eq!(send(&Event::Mouse(mouse::Event::ButtonPressed(Button::Left)), at(1.0, 1.0), &mut st), None, "shift selects instead");
    }

    #[test]
    fn block_buttons_and_copied_output() {
        let mut t = Term::new(20, 8);
        t.feed(b"\x1b]133;A\x07");
        t.begin_block("ls", "");
        t.feed(b"\r\n\x1b]133;C\x07a.txt   \r\nb.txt\r\n\x1b]133;D;0\x07\x1b]133;A\x07");
        t.begin_block("true", "");
        t.feed(b"\r\n\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07");
        t.begin_block("sleep 9", "");
        t.feed(b"\r\n\x1b]133;C\x07");
        let b = &t.blocks;
        assert_eq!(block_output(&t, &b[0]), "a.txt\nb.txt");
        assert_eq!(block_acts(&t, &b[0], true), [BlockAct::CopyOutput, BlockAct::CopyCommand, BlockAct::Rerun]);
        assert_eq!(block_acts(&t, &b[0], false), [BlockAct::CopyOutput, BlockAct::CopyCommand]);
        assert_eq!(block_acts(&t, &b[1], true), [BlockAct::CopyCommand, BlockAct::Rerun]); // no output
        assert!(block_acts(&t, &b[2], true).is_empty()); // still running
    }

    // Regression: ISSUE-001 — block characters (Claude's logo, progress bars) drawn as font glyphs left gaps
    // Found by /qa on 2026-11-15
    // Report: .gstack/qa-reports/run-20261115T001718Z/qa-report-termpad-2026-11-15.md
    #[test]
    fn block_elements_fill_exact_cell_fractions() {
        assert_eq!(block_shape('█'), Some((1.0, vec![[0.0, 0.0, 1.0, 1.0]])));
        assert_eq!(block_shape('▀'), Some((1.0, vec![[0.0, 0.0, 1.0, 0.5]])));
        assert_eq!(block_shape('▂'), Some((1.0, vec![[0.0, 0.75, 1.0, 1.0]])));
        assert_eq!(block_shape('▐'), Some((1.0, vec![[0.5, 0.0, 1.0, 1.0]])));
        assert_eq!(block_shape('▊'), Some((1.0, vec![[0.0, 0.0, 0.75, 1.0]])));
        assert_eq!(block_shape('▛'), Some((1.0, vec![[0.0, 0.0, 0.5, 0.5], [0.5, 0.0, 1.0, 0.5], [0.0, 0.5, 0.5, 1.0]])));
        assert_eq!(block_shape('▒'), Some((0.5, vec![[0.0, 0.0, 1.0, 1.0]])));
        assert_eq!(block_shape('a'), None);
        assert_eq!(block_shape('─'), None);
    }
}
