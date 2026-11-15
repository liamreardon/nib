//! Pure logic: which keys the editor keeps, which go to the running program, and what "send" writes.

#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Ret,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Char(char),
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    Edit,            // let the text editor handle it (cursor moves, typing, selection)
    Send,            // write the editor's text to the program, then Return
    Forward(Vec<u8>), // raw bytes straight to the program
    HistoryPrev,
    HistoryNext,
    Complete,        // Tab mid-draft: finish the word
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Ctx {
    pub shift: bool,
    pub ctrl: bool,
    pub editor_empty: bool,
    pub at_shell_prompt: bool,
    pub app_cursor: bool,
    pub browsing_history: bool,
}

pub fn route(key: &Key, c: &Ctx) -> KeyAction {
    if c.ctrl {
        if let Key::Char(ch) = key {
            let a = ch.to_ascii_lowercase();
            if a.is_ascii_lowercase() {
                // Ctrl-C must always interrupt; other Ctrl keys only reach the program when there's no draft.
                return if a == 'c' || c.editor_empty { KeyAction::Forward(vec![a as u8 & 0x1f]) } else { KeyAction::Edit };
            }
        }
    }
    match key {
        Key::Ret if c.shift => return KeyAction::Edit,
        Key::Ret => return KeyAction::Send,
        Key::Escape => return KeyAction::Forward(vec![0x1b]), // Esc interrupts agents like Claude even mid-draft
        Key::Tab if c.shift => return KeyAction::Forward(b"\x1b[Z".to_vec()), // Shift-Tab: Claude Code mode switch
        _ => {}
    }
    if c.at_shell_prompt && (c.editor_empty || c.browsing_history) {
        match key {
            Key::Up => return KeyAction::HistoryPrev,
            Key::Down => return KeyAction::HistoryNext,
            _ => {}
        }
    }
    if !c.editor_empty {
        return if *key == Key::Tab { KeyAction::Complete } else { KeyAction::Edit };
    }
    let csi: &[u8] = if c.app_cursor { b"\x1bO" } else { b"\x1b[" };
    let arrow = |f: u8| KeyAction::Forward([csi, &[f]].concat());
    match key {
        Key::Up => arrow(b'A'),
        Key::Down => arrow(b'B'),
        Key::Right => arrow(b'C'),
        Key::Left => arrow(b'D'),
        Key::Tab => KeyAction::Forward(vec![0x09]),
        Key::Backspace => KeyAction::Forward(vec![0x7f]),
        _ => KeyAction::Edit,
    }
}

/// Bytes for a key in full-screen apps (vim, less, top), where every key goes straight to the program.
pub fn encode_raw(key: &Key, text: Option<&str>, shift: bool, ctrl: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let ss3 = |f: u8| if app_cursor { vec![0x1b, b'O', f] } else { vec![0x1b, b'[', f] };
    Some(match key {
        Key::Ret => vec![b'\r'],
        Key::Tab if shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![b'\t'],
        Key::Backspace => vec![0x7f],
        Key::Escape => vec![0x1b],
        Key::Up => ss3(b'A'),
        Key::Down => ss3(b'B'),
        Key::Right => ss3(b'C'),
        Key::Left => ss3(b'D'),
        Key::Home => ss3(b'H'),
        Key::End => ss3(b'F'),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Char(c) if ctrl && c.is_ascii_alphabetic() => vec![c.to_ascii_lowercase() as u8 & 0x1f],
        _ => text.filter(|t| !t.is_empty())?.as_bytes().to_vec(),
    })
}

/// The paste part and the Return part are separate so the caller can leave a gap between them.
pub fn payload(text: &str, bracketed: bool) -> (Vec<u8>, Vec<u8>) {
    if text.is_empty() {
        return (vec![], vec![b'\r']);
    }
    if bracketed {
        // Strip ESC so the text can't close the paste early and smuggle in keystrokes.
        let clean = text.replace('\x1b', "");
        return (format!("\x1b[200~{clean}\x1b[201~").into_bytes(), vec![b'\r']);
    }
    (text.replace('\n', "\r").into_bytes(), vec![b'\r'])
}

#[derive(Default)]
pub struct History {
    pub entries: Vec<String>,
    index: Option<usize>, // None = not browsing
}

impl History {
    pub fn add(&mut self, s: &str) {
        if !s.is_empty() && self.entries.last().map(String::as_str) != Some(s) {
            self.entries.push(s.to_string());
        }
        self.index = None;
    }

    pub fn prev(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let i = self.index.unwrap_or(self.entries.len()).saturating_sub(1);
        self.index = Some(i);
        Some(self.entries[i].clone())
    }

    /// Past the newest entry returns "" (back to an empty editor); when not browsing returns None.
    pub fn next(&mut self) -> Option<String> {
        let i = self.index?;
        if i + 1 < self.entries.len() {
            self.index = Some(i + 1);
            return Some(self.entries[i + 1].clone());
        }
        self.index = None;
        Some(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyAction::*;

    fn r(key: Key, c: Ctx) -> KeyAction {
        route(&key, &c)
    }
    fn empty() -> Ctx {
        Ctx { editor_empty: true, ..Default::default() }
    }
    fn draft() -> Ctx {
        Ctx::default()
    }

    #[test]
    fn send_wraps_in_bracketed_paste_then_return() {
        assert_eq!(payload("ls -la", true), (b"\x1b[200~ls -la\x1b[201~".to_vec(), b"\r".to_vec()));
    }

    #[test]
    fn send_strips_escapes_so_text_cannot_end_the_paste() {
        assert_eq!(payload("a\x1b[201~rm -rf /", true).0, b"\x1b[200~a[201~rm -rf /\x1b[201~".to_vec());
    }

    #[test]
    fn send_without_bracketed_paste_turns_newlines_into_returns() {
        assert_eq!(payload("a\nb", false), (b"a\rb".to_vec(), b"\r".to_vec()));
        assert_eq!(payload("", true), (vec![], b"\r".to_vec()));
    }

    #[test]
    fn return_sends_shift_return_edits() {
        assert_eq!(r(Key::Ret, draft()), Send);
        assert_eq!(r(Key::Ret, Ctx { shift: true, ..draft() }), Edit);
    }

    #[test]
    fn arrows_edit_with_draft_forward_when_empty_and_program_running() {
        assert_eq!(r(Key::Up, draft()), Edit);
        assert_eq!(r(Key::Up, empty()), Forward(b"\x1b[A".to_vec()));
        assert_eq!(r(Key::Down, Ctx { app_cursor: true, ..empty() }), Forward(b"\x1bOB".to_vec()));
    }

    #[test]
    fn up_down_recall_history_at_shell_prompt_and_keep_browsing() {
        let prompt = Ctx { at_shell_prompt: true, ..empty() };
        assert_eq!(r(Key::Up, prompt), HistoryPrev);
        assert_eq!(r(Key::Down, prompt), HistoryNext);
        let browsing = Ctx { at_shell_prompt: true, browsing_history: true, ..draft() };
        assert_eq!(r(Key::Up, browsing), HistoryPrev);
        assert_eq!(r(Key::Left, browsing), Edit);
    }

    #[test]
    fn escape_ctrl_c_and_shift_tab_always_reach_the_program() {
        assert_eq!(r(Key::Escape, draft()), Forward(vec![0x1b]));
        assert_eq!(r(Key::Char('c'), Ctx { ctrl: true, ..draft() }), Forward(vec![0x03]));
        assert_eq!(r(Key::Tab, Ctx { shift: true, ..draft() }), Forward(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn other_ctrl_keys_tab_backspace_forward_only_when_empty() {
        assert_eq!(r(Key::Char('d'), Ctx { ctrl: true, ..empty() }), Forward(vec![0x04]));
        assert_eq!(r(Key::Char('a'), Ctx { ctrl: true, ..draft() }), Edit);
        assert_eq!(r(Key::Tab, empty()), Forward(vec![0x09]));
        assert_eq!(r(Key::Tab, draft()), Complete);
        assert_eq!(r(Key::Backspace, empty()), Forward(vec![0x7f]));
        assert_eq!(r(Key::Backspace, draft()), Edit);
    }

    #[test]
    fn raw_keys_for_full_screen_apps() {
        assert_eq!(encode_raw(&Key::Char('x'), Some("x"), false, false, false), Some(b"x".to_vec()));
        assert_eq!(encode_raw(&Key::Char('w'), None, false, true, false), Some(vec![0x17]));
        assert_eq!(encode_raw(&Key::Up, None, false, false, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_raw(&Key::PageDown, None, false, false, false), Some(b"\x1b[6~".to_vec()));
        assert_eq!(encode_raw(&Key::Other, None, false, false, false), None);
    }

    #[test]
    fn history_walks_back_and_forward_to_empty() {
        let mut h = History::default();
        assert_eq!(h.prev(), None);
        for s in ["one", "two", "two", ""] {
            h.add(s);
        }
        assert_eq!(h.entries, ["one", "two"]);
        assert_eq!(h.prev().as_deref(), Some("two"));
        assert_eq!(h.prev().as_deref(), Some("one"));
        assert_eq!(h.prev().as_deref(), Some("one"));
        assert_eq!(h.next().as_deref(), Some("two"));
        assert_eq!(h.next().as_deref(), Some(""));
        assert_eq!(h.next(), None);
    }
}
