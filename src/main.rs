mod complete;
mod direct;
mod keys;
mod pty;
mod qa;
mod render;
mod shell;
mod term;

use iced::futures::{channel::mpsc, SinkExt, StreamExt};
use iced::keyboard::{self, key::Named, Key as IKey};
use iced::widget::text_editor::{Action as EdAction, Binding, Content, Edit, KeyPress, Motion, Status};
use iced::widget::{canvas, column, container, operation, row, text, text_editor, Id, Space};
use iced::{event, window, Background, Border, Color, Element, Font, Length, Size, Subscription, Task, Theme};
use keys::{Ctx, History, Key, KeyAction};
use pty::Pty;
use render::Pos;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use term::Term;

const EDITOR: &str = "editor";
const NOWHERE: &str = "program"; // no widget has this id: focusing it un-focuses the editor
const STEP: Duration = Duration::from_millis(40); // time for a program to redraw after cursor keys
const INPUT_H: f32 = 104.0; // divider + chips row + ~3 editor lines (it scrolls past that)
const START: Size = Size::new(1000.0, 700.0);
const CHIP: Color = Color::from_rgb(0.16, 0.16, 0.19);
const DIVIDER: Color = Color::from_rgb(0.20, 0.20, 0.24);

struct App {
    term: Term,
    pty: Option<Arc<Pty>>,
    editor: Content,
    history: History,
    browsing: bool, // editor holds an unedited history recall, so Up/Down keep walking history
    scroll: f32,    // px looked back into history
    sel: Option<(Pos, Pos)>,
    copy_terminal: bool, // last selection was made in the terminal, so Cmd-C copies that
    branch: String,
    program: bool, // something other than the shell owns the terminal (claude, a build, a REPL)
    direct: bool,  // keys go straight to that program instead of the input box
    seek: Option<Seek>,
    qa: Option<qa::Run>, // self-test run (NIB_QA=<dir>)
    completions: Vec<String>, // choices from the last Tab, shown above the box
    hover: Option<usize>,     // block under the mouse, which shows its buttons
    done: Option<(usize, render::BlockAct)>, // last button clicked on that block
    cell: Size,
    window: Size,
    cache: canvas::Cache,
}

/// Walking a program's cursor to where you clicked, then maybe erasing back to `erase_to`.
struct Seek {
    goal: Pos,
    erase: bool,
    erase_to: Option<Pos>,
    then: Vec<u8>, // keys typed meanwhile, sent once the cursor lands
    last: Pos,
    stalls: u8,
    steps: u8,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Ready(Arc<Pty>),
    Output(Vec<u8>),
    Exited,
    Resized(Size),
    Edit(EdAction),
    Route(KeyAction),
    Stray { key: Key, shift: bool, ctrl: bool, cmd: bool, text: Option<String> }, // key nobody captured
    Write(Vec<u8>),
    SelStart(Pos),
    SelTo(Pos),
    SelClear,
    Release,
    SeekStep,
    PasteRaw(Option<String>),
    Tick,
    QaNext,
    QaShot(&'static str, window::Screenshot),
    QaFocus(&'static str, bool, bool), // label, wanted, got
    Scroll(f32),
    CopySel,
    Hover(Option<usize>),
    Block(usize, render::BlockAct),
    Noop,
}

impl App {
    fn new() -> (Self, Task<Msg>) {
        let mut app = Self {
            term: Term::new(80, 24),
            pty: None,
            editor: Content::new(),
            history: History::default(),
            browsing: false,
            scroll: 0.0,
            sel: None,
            copy_terminal: false,
            branch: String::new(),
            program: false,
            direct: false,
            seek: None,
            qa: qa::Run::from_env(),
            completions: vec![],
            hover: None,
            done: None,
            cell: render::cell_size(),
            window: START,
            cache: canvas::Cache::new(),
        };
        app.fit();
        let qa = if app.qa.is_some() { Task::done(Msg::QaNext) } else { Task::none() };
        (app, Task::batch([operation::focus(Id::new(EDITOR)), qa]))
    }

    /// The canvas area: the window minus the input (full-screen apps get everything).
    fn area(&self) -> Size {
        let input = if self.term.alt_screen { 0.0 } else { INPUT_H };
        Size::new(self.window.width, self.window.height - input)
    }

    /// Size the grid (and the shell's idea of it) to the space it is drawn in.
    fn fit(&mut self) {
        self.cache.clear();
        let (cols, rows) = render::grid_size(&self.term, self.area(), self.cell);
        if (cols, rows) == (self.term.cols, self.term.rows) {
            return; // no SIGWINCH, so the shell doesn't redraw its prompt for nothing
        }
        self.term.resize(cols, rows);
        if let Some(p) = &self.pty {
            p.resize(cols as u16, rows as u16);
        }
    }

    fn write(&self, bytes: &[u8]) {
        if let Some(p) = &self.pty {
            p.write(bytes);
        }
    }

    fn at_prompt(&self) -> bool {
        self.pty.as_ref().is_some_and(|p| p.at_shell_prompt())
    }

    fn ctx(&self, shift: bool, ctrl: bool) -> Ctx {
        Ctx {
            shift,
            ctrl,
            editor_empty: self.editor.text().is_empty(),
            at_shell_prompt: self.at_prompt(),
            app_cursor: self.term.app_cursor,
            browsing_history: self.browsing,
        }
    }

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Ready(p) => {
                p.resize(self.term.cols as u16, self.term.rows as u16);
                self.pty = Some(p);
            }
            Msg::Output(bytes) => {
                let mode = (self.term.alt_screen, self.term.integrated);
                self.term.feed(&bytes);
                let replies = std::mem::take(&mut self.term.replies);
                self.write(&replies);
                self.cache.clear();
                if std::mem::take(&mut self.term.prompt_seen) {
                    self.branch = git_branch(&self.term.cwd);
                }
                if (self.term.alt_screen, self.term.integrated) != mode {
                    // Full-screen apps get the whole window and every key; blocks need room for headers.
                    self.fit();
                }
                let program = self.pty.is_some() && !self.at_prompt();
                if program != self.program {
                    // A program took over: type straight into it, like any terminal. Back at the prompt: the box.
                    self.program = program;
                    self.direct = program;
                    self.seek = None;
                    return operation::focus(Id::new(if program { NOWHERE } else { EDITOR }));
                }
                if mode.0 && !self.term.alt_screen && !self.direct {
                    return operation::focus(Id::new(EDITOR));
                }
            }
            Msg::Exited => return iced::exit(),
            Msg::Resized(size) => {
                self.window = size;
                self.fit();
            }
            Msg::Edit(a) => {
                if a.is_edit() {
                    self.completions.clear();
                }
                self.direct = false; // clicked or typed in the box: it drafts, Enter pastes into the program
                if a.is_edit() {
                    self.browsing = false; // user changed a recalled command; arrows move the cursor again
                }
                self.copy_terminal = false;
                self.editor.perform(a);
            }
            Msg::Route(a) => return self.act(a),
            Msg::Stray { key, shift, ctrl, cmd, text } => {
                if self.term.alt_screen || (self.direct && self.program) {
                    return self.raw_key(key, shift, ctrl, cmd, text);
                }
                if cmd {
                    return if key == Key::Char('c') { self.update(Msg::CopySel) } else { Task::none() };
                }
                // Typing while the editor lost focus still lands in the editor, never raw in the PTY.
                let a = keys::route(&key, &self.ctx(shift, ctrl));
                let done = if a == KeyAction::Edit {
                    for c in text.unwrap_or_default().chars().filter(|c| !c.is_control()) {
                        self.editor.perform(EdAction::Edit(Edit::Insert(c)));
                        self.browsing = false;
                    }
                    Task::none()
                } else {
                    self.act(a)
                };
                return Task::batch([done, operation::focus(Id::new(EDITOR))]);
            }
            Msg::Write(bytes) => self.write(&bytes),
            Msg::SelStart(p) => {
                self.sel = Some((p, p));
                self.copy_terminal = true;
                self.cache.clear();
            }
            Msg::Release => {
                let click = self.sel.filter(|(a, b)| a == b).map(|(a, _)| a);
                if click.is_some() {
                    self.sel = None; // a click, not a drag: nothing to keep selected
                    self.cache.clear();
                }
                if self.term.alt_screen {
                    return Task::none();
                }
                if !self.program {
                    return operation::focus(Id::new(EDITOR)); // selecting output never takes the keyboard away
                }
                // In a running program: clicking its input line moves its cursor there.
                self.direct = true;
                let blur = operation::focus(Id::new(NOWHERE));
                let Some(goal) = click.and_then(|p| direct::target(&self.term, p)) else { return blur };
                self.seek = Some(Seek { goal, erase: false, erase_to: None, then: vec![], last: self.cursor(), stalls: 0, steps: 0 });
                return Task::batch([blur, self.seek_step()]);
            }
            Msg::SeekStep => {
                let cur = self.cursor();
                let Some(s) = self.seek.as_mut() else { return Task::none() };
                if cur != s.last {
                    return self.seek_step();
                }
                s.stalls += 1;
                if s.stalls < 4 {
                    return Task::perform(tokio::time::sleep(STEP), |_| Msg::SeekStep); // slow redraw: wait a bit more
                }
                // The cursor won't go there (past the end, onto a prompt mark): stop and deliver what was typed.
                let then = std::mem::take(&mut s.then);
                self.seek = None;
                self.write(&then);
            }
            Msg::PasteRaw(text) => {
                if let Some(text) = text {
                    let (paste, _) = keys::payload(&text, self.term.bracketed_paste);
                    return self.send_raw(paste);
                }
            }
            Msg::SelTo(p) => {
                if let Some((a, _)) = self.sel {
                    self.sel = Some((a, p));
                    self.cache.clear();
                }
            }
            Msg::SelClear => {
                self.sel = None;
                self.cache.clear();
                if self.program && !self.term.alt_screen {
                    self.direct = true;
                    return operation::focus(Id::new(NOWHERE));
                }
                return operation::focus(Id::new(EDITOR));
            }
            Msg::Scroll(px) => {
                if !self.term.alt_screen {
                    let max = if self.term.integrated {
                        render::content_height(&self.term, self.cell) - self.area().height
                    } else {
                        self.term.scrollback.len() as f32 * self.cell.height
                    };
                    self.scroll = (self.scroll + px).clamp(0.0, max.max(0.0));
                    self.cache.clear();
                }
            }
            Msg::CopySel => {
                if let Some(sel) = self.sel {
                    return iced::clipboard::write(render::selected_text(&self.term, sel));
                }
            }
            Msg::Tick => self.cache.clear(),
            Msg::Hover(h) => {
                if h != self.hover {
                    (self.hover, self.done) = (h, None);
                    self.cache.clear();
                }
            }
            Msg::Block(i, a) => return self.block_act(i, a),
            Msg::QaNext => return qa::next(self),
            Msg::QaShot(name, shot) => {
                if let Some(r) = &mut self.qa {
                    r.save(name, &shot);
                }
                return qa::next(self);
            }
            Msg::QaFocus(label, want, got) => {
                if let Some(r) = &mut self.qa {
                    r.log(want == got, &format!("{label} (focused: {got})"));
                }
                return qa::next(self);
            }
            Msg::Noop => {}
        }
        Task::none()
    }

    fn cursor(&self) -> Pos {
        (self.term.abs(self.term.row), self.term.col)
    }

    /// A key for the program itself (full-screen app, or direct typing into Claude's box).
    fn raw_key(&mut self, key: Key, shift: bool, ctrl: bool, cmd: bool, text: Option<String>) -> Task<Msg> {
        if cmd {
            return match key {
                Key::Char('c') => self.update(Msg::CopySel),
                Key::Char('v') => iced::clipboard::read().map(Msg::PasteRaw),
                _ => Task::none(),
            };
        }
        let bytes = if key == Key::Ret && shift && !self.term.alt_screen {
            Some(vec![b'\n']) // Ctrl-J: a new line in Claude's box instead of sending
        } else {
            keys::encode_raw(&key, text.as_deref(), shift, ctrl, self.term.app_cursor)
        };
        let Some(bytes) = bytes else { return Task::none() };
        if !self.term.alt_screen && !ctrl && matches!(key, Key::Backspace | Key::Delete | Key::Char(_)) {
            // Typing or Backspace over a selection in the input line replaces it, like any editor.
            let then = if matches!(key, Key::Char(_)) { bytes.clone() } else { vec![] };
            if let Some(t) = self.erase_selection(then) {
                return t;
            }
        }
        self.send_raw(bytes)
    }

    fn send_raw(&mut self, bytes: Vec<u8>) -> Task<Msg> {
        self.scroll = 0.0;
        match &mut self.seek {
            Some(s) => s.then.extend(bytes), // cursor still walking: these land where you clicked
            None => self.write(&bytes),
        }
        Task::none()
    }

    fn erase_selection(&mut self, then: Vec<u8>) -> Option<Task<Msg>> {
        let (a, b) = self.sel?;
        let ((l0, c0), (l1, c1)) = if a <= b { (a, b) } else { (b, a) };
        let start = direct::target(&self.term, (l0, c0))?;
        let end = direct::target(&self.term, (l1, c1 + 1))?;
        self.sel = None;
        self.cache.clear();
        let last = self.cursor();
        self.seek = Some(Seek { goal: end, erase: false, erase_to: Some(start), then, last, stalls: 0, steps: 0 });
        Some(self.seek_step())
    }

    /// Sends the next batch of cursor keys, or finishes: the erase phase next, then the keys typed meanwhile.
    fn seek_step(&mut self) -> Task<Msg> {
        let cur = self.cursor();
        let lines = direct::input_lines(&self.term);
        while let Some(s) = self.seek.as_mut() {
            let keys = if s.steps > 60 || !lines.contains(&s.goal.0) {
                None // lost the box (it moved or closed): give up, still deliver the typing
            } else {
                direct::step(&self.term, s.goal, s.erase)
            };
            if let Some(keys) = keys {
                (s.last, s.stalls, s.steps) = (cur, 0, s.steps + 1);
                if let Some(p) = &self.pty {
                    p.write(&keys);
                }
                return Task::perform(tokio::time::sleep(STEP), |_| Msg::SeekStep);
            }
            match s.erase_to.take() {
                Some(start) if s.steps <= 60 && lines.contains(&s.goal.0) => (s.goal, s.erase) = (start, true),
                _ => {
                    let then = std::mem::take(&mut s.then);
                    self.seek = None;
                    self.write(&then);
                }
            }
        }
        Task::none()
    }

    fn act(&mut self, a: KeyAction) -> Task<Msg> {
        match a {
            KeyAction::Send => return self.send(),
            KeyAction::Forward(bytes) => {
                self.scroll = 0.0;
                self.write(&bytes)
            }
            KeyAction::HistoryPrev => {
                if let Some(s) = self.history.prev() {
                    self.recall(&s)
                }
            }
            KeyAction::HistoryNext => {
                if let Some(s) = self.history.next() {
                    self.recall(&s)
                }
            }
            KeyAction::Complete => self.complete(),
            KeyAction::Edit => {}
        }
        Task::none()
    }

    fn cwd(&self) -> String {
        if self.term.cwd.is_empty() { std::env::var("HOME").unwrap_or_default() } else { self.term.cwd.clone() }
    }

    fn complete(&mut self) {
        let text = self.editor.text();
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let at = self.editor.cursor().position;
        let last = text.rsplit('\n').next().unwrap_or("");
        // ponytail: completes only with the cursor at the end of the draft, the usual place for Tab.
        if at.line + 1 < self.editor.line_count() || (at.column != last.chars().count() && at.column != last.len()) {
            return;
        }
        let path = std::env::var("PATH").unwrap_or_default();
        self.completions.clear();
        if let Some((new, shown)) = complete::complete(text, &self.cwd(), &path) {
            self.editor = Content::with_text(&new);
            self.editor.perform(EdAction::Move(Motion::DocumentEnd));
            self.completions = shown;
        }
    }

    fn recall(&mut self, s: &str) {
        self.editor = Content::with_text(s);
        self.editor.perform(EdAction::Move(Motion::DocumentEnd));
        self.browsing = !s.is_empty();
    }

    fn send(&mut self) -> Task<Msg> {
        let text = self.editor.text();
        if self.term.integrated && self.at_prompt() && !text.trim().is_empty() {
            self.term.begin_block(text.trim_end(), &self.branch); // the card shows up before the shell starts it
        }
        let (paste, enter) = keys::payload(&text, self.term.bracketed_paste);
        self.history.add(&text);
        self.browsing = false;
        self.completions.clear();
        self.editor = Content::new();
        self.scroll = 0.0;
        self.cache.clear();
        if paste.is_empty() {
            self.write(&enter);
            return Task::none();
        }
        self.write(&paste);
        // ponytail: fixed 30ms gap so TUIs (Claude Code/Ink) read the paste and the Return separately;
        // raise it if some app drops the Return.
        Task::perform(tokio::time::sleep(Duration::from_millis(30)), move |_| Msg::Write(enter))
    }

    /// A block's header button: copy its output or command, or run the command again.
    fn block_act(&mut self, i: usize, a: render::BlockAct) -> Task<Msg> {
        let Some(b) = self.term.blocks.get(i) else { return Task::none() };
        self.done = Some((i, a));
        self.cache.clear();
        match a {
            render::BlockAct::CopyOutput => iced::clipboard::write(render::block_output(&self.term, b)),
            render::BlockAct::CopyCommand => iced::clipboard::write(b.cmd.clone()),
            render::BlockAct::Rerun => {
                if self.program {
                    return Task::none();
                }
                // Sent like a typed command (history, its own block); a draft in the box survives.
                let (cmd, draft) = (b.cmd.clone(), self.editor.text());
                self.editor = Content::with_text(&cmd);
                let sent = self.send();
                let draft = draft.trim_end_matches('\n');
                if !draft.is_empty() {
                    self.recall(draft);
                    self.browsing = false;
                }
                sent
            }
        }
    }

    /// Where you are and what has the keyboard: folder, branch, and the program you're typing into.
    fn chips(&self) -> Element<'_, Msg> {
        let mut r = row![chip(render::tilde(&self.cwd()), render::FG)].spacing(6);
        if !self.branch.is_empty() {
            r = r.push(chip(format!("⎇ {}", self.branch), render::MUTED));
        }
        if self.pty.is_some() && !self.at_prompt() {
            let cmd = self.term.blocks.last().filter(|b| b.running()).map(|b| b.cmd.as_str()).unwrap_or("");
            let what = cmd.lines().next().filter(|s| !s.is_empty()).unwrap_or("program");
            r = r.push(chip(format!("typing into: {what}"), render::ACCENT));
        }
        if !self.completions.is_empty() {
            const SHOW: usize = 8;
            let mut s = self.completions.iter().take(SHOW).cloned().collect::<Vec<_>>().join("   ");
            if self.completions.len() > SHOW {
                s += &format!("   +{} more", self.completions.len() - SHOW);
            }
            return r.push(Space::new().width(Length::Fill)).push(text(s).size(12).color(render::ACCENT)).align_y(iced::Alignment::Center).into();
        }
        r.push(Space::new().width(Length::Fill))
            .push(text(if self.direct && self.program {
                "Typing goes straight in  ·  click its line to move the cursor  ·  select + type to replace"
            } else {
                "Enter sends  ·  Shift-Enter new line"
            }).size(11).color(render::MUTED))
            .align_y(iced::Alignment::Center)
            .into()
    }

    fn view(&self) -> Element<'_, Msg> {
        let screen = canvas(render::View {
            term: &self.term,
            scroll: self.scroll,
            sel: self.sel,
            cell: self.cell,
            cache: &self.cache,
            direct: self.direct && self.program,
            hover: self.hover,
            done: self.done,
            idle: !self.program,
        })
        .width(Length::Fill)
        .height(Length::Fill);
        if self.term.alt_screen {
            return screen.into();
        }
        let base = self.ctx(false, false);
        let copy_terminal = self.copy_terminal && self.sel.is_some();
        let editor = text_editor(&self.editor)
            .id(Id::new(EDITOR))
            .placeholder(if self.program {
                "Click here for a draft box. Enter pastes it into the program."
            } else {
                "Type a command. Click anywhere in it to edit."
            })
            .on_action(Msg::Edit)
            .font(render::font())
            .size(render::FONT_SIZE)
            .padding(4)
            .height(Length::Fill)
            .style(|_, _| text_editor::Style {
                background: Background::Color(Color::TRANSPARENT),
                border: Border::default(),
                placeholder: render::MUTED,
                value: render::FG,
                selection: render::SELECTION,
            })
            .key_binding(move |kp: KeyPress| {
                if !matches!(kp.status, Status::Focused { .. }) {
                    return None;
                }
                if kp.modifiers.command() {
                    if copy_terminal && matches!(kp.key.as_ref(), IKey::Character("c")) {
                        return Some(Binding::Custom(Msg::CopySel));
                    }
                    return Binding::from_key_press(kp); // Cmd-C/V/X/A and Cmd-arrows stay native
                }
                let ctx = Ctx { shift: kp.modifiers.shift(), ctrl: kp.modifiers.control(), ..base };
                match keys::route(&to_key(&kp.key), &ctx) {
                    // Swallow keys the editor has no binding for, so they never leak to the stray-key path.
                    KeyAction::Edit => Binding::from_key_press(kp).or(Some(Binding::Custom(Msg::Noop))),
                    a => Some(Binding::Custom(Msg::Route(a))),
                }
            });
        let divider = container(Space::new()).width(Length::Fill).height(1).style(|_| fill(DIVIDER));
        let input = container(column![self.chips(), editor].spacing(4))
            .padding([8, 12])
            .height(INPUT_H - 1.0);
        container(column![screen, divider, input]).style(|_| fill(render::BG)).into()
    }

    fn subscription(&self) -> Subscription<Msg> {
        let stray = event::listen_with(|e, status, _| match (e, status) {
            (iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }), event::Status::Ignored) => {
                Some(Msg::Stray {
                    key: to_key(&key),
                    shift: modifiers.shift(),
                    ctrl: modifiers.control(),
                    cmd: modifiers.command(),
                    text: text.map(|t| t.to_string()),
                })
            }
            _ => None,
        });
        Subscription::batch([
            Subscription::run(pty_stream),
            window::resize_events().map(|(_, size)| Msg::Resized(size)),
            stray,
            // Tick the running block's timer once a second; nothing runs while idle at the prompt.
            if self.term.blocks.last().is_some_and(|b| b.running() && b.start.is_some()) {
                iced::time::every(Duration::from_secs(1)).map(|_| Msg::Tick)
            } else {
                Subscription::none()
            },
        ])
    }
}

fn fill(c: Color) -> container::Style {
    container::Style { background: Some(Background::Color(c)), ..container::Style::default() }
}

fn chip<'a>(label: String, color: Color) -> Element<'a, Msg> {
    container(text(label).size(12).color(color).font(Font::DEFAULT))
        .padding([2, 8])
        .style(|_| container::Style { border: Border { radius: 4.0.into(), ..Border::default() }, ..fill(CHIP) })
        .into()
}

/// Current git branch for `dir`, read from .git/HEAD (no git process per prompt). Short sha when detached.
fn git_branch(dir: &str) -> String {
    let mut d = std::path::Path::new(dir);
    loop {
        let git = d.join(".git");
        let gitdir = if git.is_dir() {
            Some(git)
        } else {
            // Worktrees and submodules: .git is a file saying where the real one is.
            std::fs::read_to_string(&git).ok().and_then(|s| s.strip_prefix("gitdir:").map(|p| d.join(p.trim())))
        };
        if let Some(g) = gitdir {
            let head = std::fs::read_to_string(g.join("HEAD")).unwrap_or_default();
            let head = head.trim();
            return head.strip_prefix("ref: refs/heads/").map_or_else(|| head.chars().take(7).collect(), str::to_string);
        }
        match d.parent() {
            Some(p) => d = p,
            None => return String::new(),
        }
    }
}

/// Starts the shell, then streams its output. Reads block, so they run on their own thread.
fn pty_stream() -> impl iced::futures::Stream<Item = Msg> {
    iced::stream::channel(100, async |mut out: mpsc::Sender<Msg>| {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let pty = match Pty::spawn(&shell, &shell::argv(&shell), &home, 80, 24) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                eprintln!("nib: could not start {shell}: {e}");
                let _ = out.send(Msg::Exited).await;
                return;
            }
        };
        let Ok(mut reader) = pty.reader() else { return };
        let _ = out.send(Msg::Ready(pty)).await;
        let (tx, mut rx) = mpsc::unbounded::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if tx.unbounded_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        while let Some(bytes) = rx.next().await {
            if out.send(Msg::Output(bytes)).await.is_err() {
                return;
            }
        }
        let _ = out.send(Msg::Exited).await;
    })
}

fn to_key(k: &IKey) -> Key {
    match k.as_ref() {
        IKey::Named(Named::Enter) => Key::Ret,
        IKey::Named(Named::Tab) => Key::Tab,
        IKey::Named(Named::Backspace) => Key::Backspace,
        IKey::Named(Named::Escape) => Key::Escape,
        IKey::Named(Named::ArrowUp) => Key::Up,
        IKey::Named(Named::ArrowDown) => Key::Down,
        IKey::Named(Named::ArrowLeft) => Key::Left,
        IKey::Named(Named::ArrowRight) => Key::Right,
        IKey::Named(Named::Home) => Key::Home,
        IKey::Named(Named::End) => Key::End,
        IKey::Named(Named::PageUp) => Key::PageUp,
        IKey::Named(Named::PageDown) => Key::PageDown,
        IKey::Named(Named::Delete) => Key::Delete,
        IKey::Named(Named::Space) => Key::Char(' '),
        IKey::Character(s) => s.chars().next().map(Key::Char).unwrap_or(Key::Other),
        _ => Key::Other,
    }
}

fn main() -> iced::Result {
    // The shell inherits these; zsh and Claude Code need a real TERM.
    std::env::set_var("TERM", "xterm-256color");
    std::env::set_var("COLORTERM", "truecolor");
    std::env::set_var("TERM_PROGRAM", "Nib");
    // Launched from inside Claude Code? Drop its markers so `claude` in here acts like a fresh session.
    std::env::remove_var("CLAUDECODE");
    std::env::remove_var("CLAUDE_CODE_CHILD_SESSION");
    // Block marks for zsh and bash. Set before any threads start; other shells get the plain terminal.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Err(e) = shell::install(&shell) {
        eprintln!("nib: no blocks this time, could not write the shell hooks: {e}");
    }
    iced::application(App::new, App::update, App::view)
        .title(|a: &App| if a.term.title.is_empty() { "Nib".to_string() } else { a.term.title.clone() })
        .theme(|_: &App| Theme::Dark)
        .window_size(START)
        .subscription(App::subscription)
        .run()
}

#[cfg(test)]
mod tests {
    #[test]
    fn git_branch_reads_head_from_any_subfolder_and_worktree_files() {
        let root = std::env::temp_dir().join("nib-test-git");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::create_dir_all(root.join("repo/src/deep")).unwrap();
        std::fs::write(root.join("repo/.git/HEAD"), "ref: refs/heads/feature-x\n").unwrap();
        assert_eq!(super::git_branch(&root.join("repo/src/deep").to_string_lossy()), "feature-x");
        std::fs::create_dir_all(root.join("wt/real")).unwrap();
        std::fs::write(root.join("wt/real/HEAD"), "0123456789abcdef\n").unwrap();
        std::fs::write(root.join("wt/.git"), "gitdir: real\n").unwrap();
        assert_eq!(super::git_branch(&root.join("wt").to_string_lossy()), "0123456");
        assert_eq!(super::git_branch("/"), "");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
