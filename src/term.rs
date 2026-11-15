//! Our terminal screen: turns the program's output bytes into a grid of styled cells.
//! The `vte` crate only splits the byte stream into escape sequences; everything they mean is here.

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

const SCROLLBACK: usize = 10_000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style { fg: Color::Default, bg: Color::Default, bold: false, dim: false, italic: false, underline: false, inverse: false }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub marks: [char; 2], // combining marks drawn on top of `ch` (accents, variation selectors); '\0' = none
    pub style: Style,
    pub wide_tail: bool, // right half of a double-width char; draw nothing here
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', marks: ['\0'; 2], style: Style::default(), wide_tail: false }
    }
}

impl Cell {
    pub fn has_marks(&self) -> bool {
        self.marks[0] != '\0'
    }

    /// The char and its marks, as drawn or copied.
    pub fn push_to(&self, s: &mut String) {
        s.push(self.ch);
        s.extend(self.marks.iter().filter(|&&m| m != '\0'));
    }
}

pub type Row = Vec<Cell>;

/// One command and its output, found from the shell's OSC 133 marks (see shell.rs).
/// Lines are absolute: they keep their number as they scroll into history.
#[derive(Clone, Debug)]
pub struct Block {
    pub cmd: String,
    pub cwd: String,
    pub branch: String,
    pub start: Option<usize>, // first output line; None until the shell says the command started
    pub end: Option<usize>,   // one past the last output line; None while it runs
    pub exit: Option<i32>,
    pub began: Instant,
    pub took: Option<Duration>,
}

impl Block {
    fn new(cmd: &str, cwd: &str, branch: &str, start: Option<usize>) -> Self {
        Block {
            cmd: cmd.to_string(),
            cwd: cwd.to_string(),
            branch: branch.to_string(),
            start,
            end: None,
            exit: None,
            began: Instant::now(),
            took: None,
        }
    }

    pub fn running(&self) -> bool {
        self.end.is_none()
    }
}

#[derive(Clone, Copy, Default)]
struct Saved {
    row: usize,
    col: usize,
    pen: Style,
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    lines: Vec<Row>,      // the visible screen
    other: Vec<Row>,      // the inactive screen (primary while alt is up, and vice versa)
    pub scrollback: VecDeque<Row>,
    pub row: usize,
    pub col: usize,
    wrap_pending: bool,
    pen: Style,
    saved: Saved,
    saved_alt: Saved,
    top: usize,
    bottom: usize,
    pub alt_screen: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    autowrap: bool,
    graphics: [bool; 2], // G0/G1 set to DEC line drawing (ESC ( 0, ESC ) 0)
    shifted: bool,       // SO picked G1, SI goes back to G0
    pub title: String,
    pub replies: Vec<u8>, // answers to queries (cursor position, device attributes) to write back
    parser: Parser,
    pushed: usize, // lines ever moved into scrollback; absolute line = pushed + screen row
    pub blocks: Vec<Block>,
    pub integrated: bool,  // the shell sends block marks; without them we draw a plain terminal
    pub cwd: String,
    pub prompt_seen: bool, // a new prompt appeared since the app last looked (it refreshes git info)
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Term {
            cols,
            rows,
            lines: vec![vec![Cell::default(); cols]; rows],
            other: vec![vec![Cell::default(); cols]; rows],
            scrollback: VecDeque::new(),
            row: 0,
            col: 0,
            wrap_pending: false,
            pen: Style::default(),
            saved: Saved::default(),
            saved_alt: Saved::default(),
            top: 0,
            bottom: rows - 1,
            alt_screen: false,
            app_cursor: false,
            bracketed_paste: false,
            cursor_visible: true,
            autowrap: true,
            graphics: [false; 2],
            shifted: false,
            title: String::new(),
            replies: Vec::new(),
            parser: Parser::new(),
            pushed: 0,
            blocks: Vec::new(),
            integrated: false,
            cwd: String::new(),
            prompt_seen: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    pub fn screen(&self) -> &[Row] {
        &self.lines
    }

    /// Absolute number of a screen row.
    pub fn abs(&self, row: usize) -> usize {
        self.pushed + row
    }

    /// Oldest absolute line still kept.
    pub fn first_line(&self) -> usize {
        self.pushed - self.scrollback.len()
    }

    pub fn line(&self, abs: usize) -> Option<&Row> {
        if abs >= self.pushed {
            self.lines.get(abs - self.pushed)
        } else {
            self.scrollback.get(abs.checked_sub(self.first_line())?)
        }
    }

    /// Called when the user sends a command at the prompt, so its block shows up before the shell
    /// even starts it (slow preexec hooks can take seconds).
    pub fn begin_block(&mut self, cmd: &str, branch: &str) {
        let b = Block::new(cmd, &self.cwd, branch, None);
        self.blocks.push(b);
    }

    /// Where the cursor is, as an absolute line; output that ended mid-line still counts that line.
    fn mark_line(&self) -> usize {
        self.abs(self.row) + usize::from(self.col > 0)
    }

    fn prompt_mark(&mut self) {
        self.prompt_seen = true;
        // A command sent but never started (blank line, comment) leaves an empty pending block.
        if self.blocks.last().is_some_and(|b| b.start.is_none()) {
            self.blocks.pop();
        }
        if !self.integrated {
            self.integrated = true;
            // Whatever the shell printed before its first prompt (errors, motd) becomes a "startup" block.
            let (from, to) = (self.first_line(), self.mark_line());
            if (from..to).any(|l| self.line(l).is_some_and(|r| r.iter().any(|c| c.ch != ' '))) {
                let mut b = Block::new("", &self.cwd, "", Some(from));
                b.end = Some(to);
                self.blocks.push(b);
            }
        }
    }

    fn command_mark(&mut self) {
        let line = self.abs(self.row);
        match self.blocks.last_mut() {
            Some(b) if b.start.is_none() => {
                b.start = Some(line);
                b.began = Instant::now();
            }
            _ => {
                let b = Block::new("", &self.cwd, "", Some(line)); // command not sent from our editor
                self.blocks.push(b);
            }
        }
    }

    fn finished_mark(&mut self, exit: Option<i32>) {
        let end = self.mark_line();
        if let Some(b) = self.blocks.last_mut().filter(|b| b.start.is_some() && b.end.is_none()) {
            b.end = Some(end.max(b.start.unwrap_or(0)));
            b.exit = exit;
            b.took = Some(b.began.elapsed());
        }
    }

    /// Plain text of one visible row, trailing spaces trimmed.
    #[cfg(test)]
    pub fn row_text(&self, r: usize) -> String {
        row_to_string(&self.lines[r])
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        for screen in [&mut self.lines, &mut self.other] {
            for line in screen.iter_mut() {
                line.resize(cols, Cell::default());
            }
        }
        // Shrinking: drop rows from the top into scrollback while the cursor is near the bottom,
        // otherwise from the bottom, so the line being typed on stays visible.
        while self.lines.len() > rows {
            if self.row > 0 && self.row + 1 >= self.lines.len() {
                let gone = self.lines.remove(0);
                if !self.alt_screen {
                    self.push_scrollback(gone);
                }
                self.row -= 1;
            } else {
                self.lines.pop();
            }
        }
        self.lines.resize(rows, vec![Cell::default(); cols]);
        self.other.resize(rows, vec![Cell::default(); cols]);
        self.other.truncate(rows);
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.wrap_pending = false;
    }

    fn blank(&self) -> Cell {
        // Erased cells keep the current background (what full-screen apps expect).
        Cell { style: Style { bg: self.pen.bg, ..Style::default() }, ..Cell::default() }
    }

    fn blank_row(&self) -> Row {
        vec![self.blank(); self.cols]
    }

    fn push_scrollback(&mut self, row: Row) {
        if self.scrollback.len() == SCROLLBACK {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
        self.pushed += 1;
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            let gone = self.lines.remove(self.top);
            if self.top == 0 && !self.alt_screen {
                self.push_scrollback(gone);
            }
            let blank = self.blank_row();
            self.lines.insert(self.bottom, blank);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            self.lines.remove(self.bottom);
            let blank = self.blank_row();
            self.lines.insert(self.top, blank);
        }
    }

    fn linefeed(&mut self) {
        if self.row == self.bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn put_char(&mut self, c: char) {
        let w = c.width().unwrap_or(0);
        if w == 0 {
            return self.add_mark(c);
        }
        if self.wrap_pending && self.autowrap {
            self.col = 0;
            self.linefeed();
        }
        self.wrap_pending = false;
        if w == 2 && self.col + 1 >= self.cols {
            if !self.autowrap {
                return;
            }
            self.lines[self.row][self.col] = self.blank();
            self.col = 0;
            self.linefeed();
        }
        let style = self.pen;
        self.lines[self.row][self.col] = Cell { ch: c, style, ..Cell::default() };
        if w == 2 {
            self.lines[self.row][self.col + 1] = Cell { style, wide_tail: true, ..Cell::default() };
        }
        if self.col + w >= self.cols {
            self.col = self.cols - 1;
            self.wrap_pending = true;
        } else {
            self.col += w;
        }
    }

    /// A zero-width char goes on the char just printed: the cell before the cursor, or the cursor's
    /// own cell when it sits at the right edge waiting to wrap.
    fn add_mark(&mut self, m: char) {
        let col = if self.wrap_pending { self.col } else if self.col > 0 { self.col - 1 } else { return };
        let row = &mut self.lines[self.row];
        let col = if row[col].wide_tail { col.saturating_sub(1) } else { col };
        // ponytail: two marks per cell, later ones dropped; enough for stacked Vietnamese accents.
        if let Some(slot) = row[col].marks.iter_mut().find(|m| **m == '\0') {
            *slot = m;
        }
    }

    fn erase(&mut self, row: usize, from: usize, to: usize) {
        let b = self.blank();
        for c in from..to.min(self.cols) {
            self.lines[row][c] = b;
        }
    }

    fn set_alt(&mut self, on: bool, save_cursor: bool) {
        if on == self.alt_screen {
            return;
        }
        if on && save_cursor {
            self.saved_alt = Saved { row: self.row, col: self.col, pen: self.pen };
        }
        std::mem::swap(&mut self.lines, &mut self.other);
        self.alt_screen = on;
        if on {
            let blank = self.blank_row();
            self.lines.iter_mut().for_each(|l| *l = blank.clone());
        } else if save_cursor {
            let s = self.saved_alt;
            (self.row, self.col, self.pen) = (s.row.min(self.rows - 1), s.col.min(self.cols - 1), s.pen);
        }
        self.wrap_pending = false;
    }

    fn sgr(&mut self, params: &Params) {
        let mut it = params.iter();
        if params.is_empty() {
            self.pen = Style::default();
            return;
        }
        while let Some(p) = it.next() {
            match p[0] {
                0 => self.pen = Style::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                3 => self.pen.italic = true,
                4 => self.pen.underline = p.get(1) != Some(&0),
                7 => self.pen.inverse = true,
                22 => (self.pen.bold, self.pen.dim) = (false, false),
                23 => self.pen.italic = false,
                24 => self.pen.underline = false,
                27 => self.pen.inverse = false,
                n @ 30..=37 => self.pen.fg = Color::Idx((n - 30) as u8),
                n @ 40..=47 => self.pen.bg = Color::Idx((n - 40) as u8),
                n @ 90..=97 => self.pen.fg = Color::Idx((n - 90 + 8) as u8),
                n @ 100..=107 => self.pen.bg = Color::Idx((n - 100 + 8) as u8),
                39 => self.pen.fg = Color::Default,
                49 => self.pen.bg = Color::Default,
                n @ (38 | 48) => {
                    // Both "38;5;n" (separate params) and "38:5:n" (sub-params) forms.
                    let rest: Vec<u16> = if p.len() > 1 {
                        p[1..].to_vec()
                    } else {
                        let kind = it.next().map(|x| x[0]);
                        let count = if kind == Some(5) { 1 } else { 3 };
                        let mut v = vec![kind.unwrap_or(0)];
                        v.extend((0..count).filter_map(|_| it.next().map(|x| x[0])));
                        v
                    };
                    let color = match rest.as_slice() {
                        [5, i, ..] => Some(Color::Idx(*i as u8)),
                        [2, r, g, b] | [2, _, r, g, b] => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
                        _ => None,
                    };
                    if let Some(c) = color {
                        if n == 38 { self.pen.fg = c } else { self.pen.bg = c }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
pub fn row_to_string(row: &Row) -> String {
    let mut s = String::new();
    row.iter().filter(|c| !c.wide_tail).for_each(|c| c.push_to(&mut s));
    s.trim_end().to_string()
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        let c = if self.graphics[usize::from(self.shifted)] { dec_graphics(c) } else { c };
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.col = self.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => self.col = ((self.col / 8 + 1) * 8).min(self.cols - 1),
            0x0a..=0x0c => {
                self.linefeed();
                self.wrap_pending = false;
            }
            0x0d => {
                self.col = 0;
                self.wrap_pending = false;
            }
            0x0e => self.shifted = true,
            0x0f => self.shifted = false,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
        match params {
            [b"0" | b"2", title, ..] => self.title = String::from_utf8_lossy(title).into_owned(),
            [b"133", b"A", ..] if !self.alt_screen => self.prompt_mark(),
            [b"133", b"C", ..] if !self.alt_screen => self.command_mark(),
            [b"133", b"D", rest @ ..] if !self.alt_screen => {
                let exit = rest.first().and_then(|c| std::str::from_utf8(c).ok()?.parse().ok());
                self.finished_mark(exit)
            }
            [b"7", url, ..] => {
                // file://host/path -> /path
                let u = String::from_utf8_lossy(url);
                if let Some(rest) = u.strip_prefix("file://") {
                    self.cwd = rest.find('/').map(|i| rest[i..].to_string()).unwrap_or_default();
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let ps: Vec<usize> = params.iter().map(|p| p[0] as usize).collect();
        let arg = |i: usize, default: usize| ps.get(i).copied().filter(|&v| v != 0).unwrap_or(default);
        let n = arg(0, 1);
        let private = intermediates.first() == Some(&b'?');
        if !intermediates.is_empty() && !private {
            if intermediates == b">" && action == 'c' {
                self.replies.extend_from_slice(b"\x1b[>0;0;0c");
            }
            return;
        }
        if action != 'm' {
            self.wrap_pending = false;
        }
        let (rows, cols) = (self.rows, self.cols);
        match action {
            'A' => self.row = self.row.saturating_sub(n).max(if self.row >= self.top { self.top } else { 0 }),
            'B' | 'e' => self.row = (self.row + n).min(if self.row <= self.bottom { self.bottom } else { rows - 1 }),
            'C' | 'a' => self.col = (self.col + n).min(cols - 1),
            'D' => self.col = self.col.saturating_sub(n),
            'E' => (self.row, self.col) = ((self.row + n).min(rows - 1), 0),
            'F' => (self.row, self.col) = (self.row.saturating_sub(n), 0),
            'G' | '`' => self.col = (n - 1).min(cols - 1),
            'd' => self.row = (n - 1).min(rows - 1),
            'H' | 'f' => (self.row, self.col) = ((arg(0, 1) - 1).min(rows - 1), (arg(1, 1) - 1).min(cols - 1)),
            'J' => match ps.first().copied().unwrap_or(0) {
                0 => {
                    self.erase(self.row, self.col, cols);
                    (self.row + 1..rows).for_each(|r| self.erase(r, 0, cols));
                }
                1 => {
                    (0..self.row).for_each(|r| self.erase(r, 0, cols));
                    self.erase(self.row, 0, self.col + 1);
                }
                2 => (0..rows).for_each(|r| self.erase(r, 0, cols)),
                3 => {
                    self.scrollback.clear();
                    self.blocks.clear(); // `clear` wipes history, so it wipes the blocks too
                }
                _ => {}
            },
            'K' => match ps.first().copied().unwrap_or(0) {
                0 => self.erase(self.row, self.col, cols),
                1 => self.erase(self.row, 0, self.col + 1),
                2 => self.erase(self.row, 0, cols),
                _ => {}
            },
            'L' | 'M' if (self.top..=self.bottom).contains(&self.row) => {
                let top = std::mem::replace(&mut self.top, self.row);
                if action == 'L' { self.scroll_down(n) } else { self.scroll_up_no_history(n) }
                self.top = top;
                self.col = 0;
            }
            '@' => {
                let b = self.blank();
                let line = &mut self.lines[self.row];
                for _ in 0..n.min(cols - self.col) {
                    line.pop();
                    line.insert(self.col, b);
                }
            }
            'P' => {
                let b = self.blank();
                let line = &mut self.lines[self.row];
                for _ in 0..n.min(cols - self.col) {
                    line.remove(self.col);
                    line.push(b);
                }
            }
            'X' => self.erase(self.row, self.col, self.col + n),
            'S' => self.scroll_up(n),
            'T' => self.scroll_down(n),
            'm' => self.sgr(params),
            'r' => {
                let (t, b) = (arg(0, 1) - 1, arg(1, rows) - 1);
                if t < b && b < rows {
                    (self.top, self.bottom) = (t, b);
                    (self.row, self.col) = (0, 0);
                }
            }
            'h' | 'l' if private => {
                let on = action == 'h';
                for &mode in &ps {
                    match mode {
                        1 => self.app_cursor = on,
                        7 => self.autowrap = on,
                        25 => self.cursor_visible = on,
                        47 | 1047 => self.set_alt(on, false),
                        1049 => self.set_alt(on, true),
                        2004 => self.bracketed_paste = on,
                        _ => {} // ponytail: mouse reporting, focus events, sync output ignored until an app needs them
                    }
                }
            }
            'n' => match ps.first() {
                Some(5) => self.replies.extend_from_slice(b"\x1b[0n"),
                Some(6) => self.replies.extend(format!("\x1b[{};{}R", self.row + 1, self.col + 1).bytes()),
                _ => {}
            },
            'c' if !private => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            's' if !private => self.saved = Saved { row: self.row, col: self.col, pen: self.pen },
            'u' if !private => self.restore(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if let [set @ (b'(' | b')')] = intermediates {
            // Only line drawing and ASCII matter today; other national sets fall back to ASCII.
            self.graphics[usize::from(*set == b')')] = byte == b'0';
            return;
        }
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved = Saved { row: self.row, col: self.col, pen: self.pen },
            b'8' => self.restore(),
            b'D' => self.linefeed(),
            b'E' => {
                self.col = 0;
                self.linefeed();
            }
            b'M' => {
                if self.row == self.top {
                    self.scroll_down(1);
                } else {
                    self.row = self.row.saturating_sub(1);
                }
            }
            b'c' => {
                let (cols, rows) = (self.cols, self.rows);
                *self = Term::new(cols, rows);
            }
            _ => {}
        }
    }
}

/// DEC special graphics: what `` ` `` to `~` mean after ESC ( 0 (boxes in older curses apps, tmux borders).
fn dec_graphics(c: char) -> char {
    const MAP: [char; 31] = [
        '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺',
        '⎻', '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    match c {
        '`'..='~' => MAP[c as usize - '`' as usize],
        _ => c,
    }
}

impl Term {
    fn restore(&mut self) {
        let s = self.saved;
        (self.row, self.col, self.pen) = (s.row.min(self.rows - 1), s.col.min(self.cols - 1), s.pen);
        self.wrap_pending = false;
    }

    /// Delete-line scrolls inside the region and never feeds scrollback.
    fn scroll_up_no_history(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            self.lines.remove(self.top);
            let blank = self.blank_row();
            self.lines.insert(self.bottom, blank);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(bytes: &str) -> Term {
        let mut t = Term::new(10, 4);
        t.feed(bytes.as_bytes());
        t
    }

    #[test]
    fn prints_and_moves_to_new_lines() {
        let t = term("ab\r\ncd");
        assert_eq!((t.row_text(0), t.row_text(1)), ("ab".into(), "cd".into()));
        assert_eq!((t.row, t.col), (1, 2));
    }

    #[test]
    fn wraps_at_the_edge_only_when_the_next_char_arrives() {
        let t = term("0123456789");
        assert_eq!((t.row, t.col), (0, 9));
        let t = term("0123456789X");
        assert_eq!((t.row_text(1), t.row, t.col), ("X".into(), 1, 1));
    }

    #[test]
    fn scrolling_off_the_bottom_fills_scrollback() {
        let t = term("1\r\n2\r\n3\r\n4\r\n5");
        assert_eq!(row_to_string(&t.scrollback[0]), "1");
        assert_eq!(t.row_text(3), "5");
    }

    #[test]
    fn cursor_moves_and_erases() {
        let t = term("hello\x1b[1;2H\x1b[K");
        assert_eq!(t.row_text(0), "h");
        let t = term("abc\r\ndef\x1b[2J");
        assert_eq!((t.row_text(0), t.row_text(1)), ("".into(), "".into()));
        let t = term("abcdef\x1b[1;3H\x1b[2P");
        assert_eq!(t.row_text(0), "abef");
    }

    #[test]
    fn redraw_up_and_overwrite_like_an_agent_ui() {
        // Ink-style redraw: move up two lines, clear each, write new content.
        let t = term("old1\r\nold2\r\n\x1b[2A\x1b[2Knew1\r\n\x1b[2Knew2");
        assert_eq!((t.row_text(0), t.row_text(1)), ("new1".into(), "new2".into()));
    }

    #[test]
    fn colors_and_reset() {
        let t = term("\x1b[1;31mR\x1b[38;5;200mX\x1b[38;2;1;2;3mY\x1b[0mZ");
        let r = &t.screen()[0];
        assert_eq!((r[0].style.fg, r[0].style.bold), (Color::Idx(1), true));
        assert_eq!(r[1].style.fg, Color::Idx(200));
        assert_eq!(r[2].style.fg, Color::Rgb(1, 2, 3));
        assert_eq!(r[3].style, Style::default());
    }

    #[test]
    fn modes_alt_screen_and_back() {
        let mut t = term("shell\x1b[?2004h\x1b[?1h");
        assert!(t.bracketed_paste && t.app_cursor);
        t.feed(b"\x1b[?1049h\x1b[HVIM"); // like vim: switch screens, then home the cursor
        assert!(t.alt_screen);
        assert_eq!(t.row_text(0), "VIM");
        t.feed(b"\x1b[?1049l");
        assert!(!t.alt_screen);
        assert_eq!(t.row_text(0), "shell");
        assert_eq!(t.col, 5);
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let t = term("日本x");
        assert_eq!(t.row_text(0), "日本x");
        assert_eq!(t.col, 5);
    }

    #[test]
    fn scroll_region_keeps_lines_outside_it() {
        let t = term("top\x1b[2;4r\x1b[4;1Ha\r\nb\r\nc");
        assert_eq!(t.row_text(0), "top");
        assert_eq!(t.row_text(3), "c");
        assert!(t.scrollback.is_empty());
    }

    #[test]
    fn answers_cursor_position_queries() {
        let t = term("ab\x1b[6n");
        assert_eq!(t.replies, b"\x1b[1;3R");
    }

    fn block_lines(t: &Term, b: &Block) -> Vec<String> {
        (b.start.unwrap()..b.end.unwrap()).map(|l| row_to_string(t.line(l).unwrap())).collect()
    }

    #[test]
    fn blocks_come_from_prompt_and_command_marks() {
        let mut t = Term::new(20, 8);
        t.feed(b"motd\r\n\x1b]7;file://host/Users/me/x\x07\x1b]133;A\x07$ ");
        assert!(t.integrated);
        assert_eq!(t.cwd, "/Users/me/x");
        t.begin_block("ls", "main");
        t.feed(b"ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        t.begin_block("false", "main");
        t.feed(b"false\r\n\x1b]133;C\x07\x1b]133;D;1\x07\x1b]133;A\x07$ ");
        let b = &t.blocks;
        assert_eq!(b.len(), 3);
        assert_eq!(block_lines(&t, &b[0]), ["motd"]); // startup block
        assert_eq!((b[1].cmd.as_str(), b[1].exit, b[1].branch.as_str()), ("ls", Some(0), "main"));
        assert_eq!(block_lines(&t, &b[1]), ["a", "b"]);
        assert_eq!((b[2].exit, block_lines(&t, &b[2]).len()), (Some(1), 0));
    }

    #[test]
    fn blocks_survive_scrolling_and_partial_last_lines() {
        let mut t = Term::new(20, 3);
        t.feed(b"\x1b]133;A\x07");
        t.begin_block("seq", "");
        t.feed(b"\r\n\x1b]133;C\x071\r\n2\r\n3\r\n4\r\nno-newline\x1b]133;D;0\x07");
        assert_eq!(block_lines(&t, &t.blocks[0]), ["1", "2", "3", "4", "no-newline"]);
    }

    #[test]
    fn unstarted_commands_vanish_and_clear_wipes_blocks() {
        let mut t = Term::new(20, 4);
        t.feed(b"\x1b]133;A\x07");
        t.begin_block("   ", "");
        t.feed(b"\x1b]133;A\x07");
        assert!(t.blocks.is_empty());
        t.begin_block("clear", "");
        t.feed(b"\r\n\x1b]133;C\x07\x1b[H\x1b[2J\x1b[3J\x1b]133;D;0\x07");
        assert!(t.blocks.is_empty());
    }

    #[test]
    fn line_drawing_charset_maps_to_box_chars() {
        assert_eq!(term("\x1b(0lqk\x1b(Bq").row_text(0), "┌─┐q");
        // G1 set to line drawing, picked with SO and dropped with SI.
        assert_eq!(term("\x1b)0x\x0ex\x0fx").row_text(0), "x│x");
    }

    #[test]
    fn combining_marks_join_the_char_before_them() {
        let t = term("e\u{301}x");
        assert_eq!((t.row_text(0), t.col), ("e\u{301}x".into(), 2));
        // After a wide char, and at the right edge where the cursor waits to wrap.
        assert_eq!(term("日\u{308}").screen()[0][0].marks[0], '\u{308}');
        let t = term("012345678e\u{301}\u{323}");
        assert_eq!(t.screen()[0][9].marks, ['\u{301}', '\u{323}']);
        assert_eq!(t.row, 0);
        // Nothing before it on the line: dropped, cursor stays put.
        assert_eq!(term("\u{301}a").row_text(0), "a");
    }

    #[test]
    fn resize_keeps_the_cursor_line_visible() {
        let mut t = term("1\r\n2\r\n3\r\n4");
        t.resize(10, 2);
        assert_eq!((t.row_text(0), t.row_text(1), t.row), ("3".into(), "4".into(), 1));
    }
}
