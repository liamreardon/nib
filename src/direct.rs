//! Editing inside a running program's own input line (Claude's ❯ box, a REPL, a nested shell).
//! We only see its screen, so a click walks the program's cursor there with arrow keys, and a
//! selection is erased with Backspace. Each step sends a count that can't overshoot, then the app
//! looks where the cursor landed and steps again.

use crate::term::{Row, Term};
use std::ops::Range;

pub type Pos = (usize, usize); // (absolute line, column)

fn is_blank(r: &Row) -> bool {
    r.iter().all(|c| c.ch == ' ')
}

/// A row drawn as a horizontal line, like the edges of Claude's input box.
fn is_rule(r: &Row) -> bool {
    let n = r.iter().filter(|c| c.ch != ' ').count();
    let lines = r.iter().filter(|c| matches!(c.ch, '─' | '━' | '═' | '╭' | '╮' | '╰' | '╯')).count();
    lines >= 3 && lines * 2 >= n
}

/// First column of typed text: past leading spaces and prompt marks (❯, >>>, │).
fn indent(r: &Row) -> usize {
    // Claude puts a no-break space after its ❯.
    r.iter().position(|c| !matches!(c.ch, ' ' | '\u{a0}' | '❯' | '›' | '>' | '│' | '┃')).unwrap_or(r.len())
}

/// One past the last character on the row.
fn text_end(r: &Row) -> usize {
    r.iter().rposition(|c| c.ch != ' ').map_or(0, |i| i + 1)
}

/// Characters (not cells: a wide char is one keypress) in columns [a, b).
fn chars(r: &Row, a: usize, b: usize) -> usize {
    let b = b.min(r.len());
    r[a.min(b)..b].iter().filter(|c| !c.wide_tail).count()
}

/// The lines of the input the cursor is in: rows touching the cursor row, up to a blank row or a rule.
pub fn input_lines(t: &Term) -> Range<usize> {
    let ok = |l: usize| t.line(l).is_some_and(|r| !is_blank(r) && !is_rule(r));
    let cur = t.abs(t.row);
    if !ok(cur) && !t.line(cur).is_some_and(|r| is_blank(r)) {
        return cur..cur; // cursor parked on a rule or off screen: no input here
    }
    let (top, last) = (t.abs(0), t.abs(t.rows - 1));
    let mut a = cur;
    while a > top && ok(a - 1) {
        a -= 1;
    }
    let mut b = cur;
    while b < last && ok(b + 1) {
        b += 1;
    }
    a..b + 1
}

/// Where a click at `p` should put the cursor, or None when it isn't on the program's input.
pub fn target(t: &Term, p: Pos) -> Option<Pos> {
    if !input_lines(t).contains(&p.0) {
        return None;
    }
    let r = t.line(p.0)?;
    let (lo, hi) = (indent(r), text_end(r));
    Some((p.0, p.1.clamp(lo, hi.max(lo))))
}

/// Keys for the next step from the cursor toward `goal`; None once there. Erasing never goes forward.
pub fn step(t: &Term, goal: Pos, erase: bool) -> Option<Vec<u8>> {
    let cur = (t.abs(t.row), t.col);
    let back = goal < cur;
    if cur == goal || (erase && !back) {
        return None;
    }
    let r = t.line(cur.0)?;
    let n = if goal.0 == cur.0 {
        chars(r, goal.1.min(cur.1), goal.1.max(cur.1))
    } else if back {
        chars(r, indent(r), cur.1) // to the start of this row; the next step crosses the line break
    } else {
        chars(r, cur.1, text_end(r))
    };
    let key: &[u8] = match (erase, back, t.app_cursor) {
        (true, ..) => b"\x7f",
        (false, true, false) => b"\x1b[D",
        (false, true, true) => b"\x1bOD",
        (false, false, false) => b"\x1b[C",
        (false, false, true) => b"\x1bOC",
    };
    Some(key.repeat(n.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Claude-like screen: rule, two-line draft, rule, footer. Cursor at row 2, col 8.
    fn claude() -> Term {
        let mut t = Term::new(40, 8);
        t.feed("output above\r\n────────────────────\r\n❯\u{a0}hello world\r\n  second line\r\n────────────────────\r\n  footer".as_bytes());
        t.feed(b"\x1b[4;9H"); // 1-based CSI: row index 3, col 8
        t
    }

    #[test]
    fn finds_the_input_box_between_the_rules() {
        let t = claude();
        assert_eq!((t.row, t.col), (3, 8));
        assert_eq!(input_lines(&t), t.abs(2)..t.abs(4));
        assert_eq!(target(&t, (t.abs(2), 30)), Some((t.abs(2), 13)), "past the end clamps to the text end");
        assert_eq!(target(&t, (t.abs(2), 0)), Some((t.abs(2), 2)), "the ❯ mark clamps to the first char");
        assert_eq!(target(&t, (t.abs(1), 3)), None, "rule");
        assert_eq!(target(&t, (t.abs(5), 3)), None, "footer");
        assert_eq!(target(&t, (t.abs(0), 3)), None, "output above");
    }

    #[test]
    fn steps_never_overshoot() {
        let t = claude();
        let row = t.abs(3);
        assert_eq!(step(&t, (row, 4), false), Some(b"\x1b[D".repeat(4)));
        assert_eq!(step(&t, (row, 12), false), Some(b"\x1b[C".repeat(4)));
        assert_eq!(step(&t, (row, 8), false), None);
        // Up a row: only back to this row's first char (6 chars), never into the row above.
        assert_eq!(step(&t, (t.abs(2), 5), false), Some(b"\x1b[D".repeat(6)));
        assert_eq!(step(&t, (t.abs(2), 5), true), Some(b"\x7f".repeat(6)));
        assert_eq!(step(&t, (row, 12), true), None, "erasing never moves forward");
    }
}
