//! Self-test: `NIB_QA=<dir> nib` drives the real app (real zsh, real Claude Code) through the
//! script below, saving window snapshots (`<name>-<w>x<h>.rgba`) and a PASS/FAIL log (`qa.log`) in
//! <dir>. It's how the UI gets checked without screen access. Claude gets no prompt: only typing,
//! clicks, one paste and Ctrl-C, never Enter.
// ponytail: one fixed script; take a script file argument if more scenarios are needed.

use crate::keys::{Key, KeyAction};
use crate::{App, Msg, EDITOR};
use iced::widget::text_editor::{Action as EdAction, Edit};
use iced::widget::{operation, Id};
use iced::{window, Point, Task};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

type Cond = fn(&App) -> bool;

#[derive(Clone, Copy)]
enum Step {
    Wait(u64),
    Until(&'static str, Cond, u64), // label, condition, timeout ms
    Check(&'static str, Cond),
    EditorFocused(&'static str, bool),
    Cmd(&'static str),        // typed into the input box, then Enter
    Type(&'static str),       // typed into the input box
    Tab,
    Send,
    Esc,
    Enter,                    // raw Return, for full-screen apps
    Keys(&'static str),       // typed with the box unfocused (straight to the program)
    CtrlC,
    ClickText(&'static str),  // click on the first character of this text in the live screen
    ClickAfter(&'static str), // click just past the end of this text
    SelectText(&'static str), // drag across this text
    SelectSpan(&'static str, &'static str), // drag from the start of one text to the end of another
    ShiftEnter,
    ClickBox,
    Paste(&'static str),
    Shot(&'static str),
}
use Step::*;

fn script() -> Vec<Step> {
    vec![
        Until("shell sends block marks", |a| a.term.integrated, 15_000),
        EditorFocused("input box has the keyboard at the prompt", true),
        Cmd(concat!("cd ", env!("CARGO_MANIFEST_DIR"))),
        Until("cd finished", finished, 5_000),
        Cmd("ls"),
        Until("ls finished", finished, 5_000),
        Check("ls block exit 0", |a| last_cmd(a, 0).is_some_and(|b| b.cmd == "ls" && b.exit == Some(0))),
        Cmd("false"),
        Until("false finished", finished, 5_000),
        Check("false block exit 1", |a| last_cmd(a, 0).is_some_and(|b| b.cmd == "false" && b.exit == Some(1))),
        Type("ls Car"),
        Tab,
        Check("Tab extends to the shared part and lists both", |a| draft(a) == "ls Cargo." && a.completions.len() == 2),
        Type("t"),
        Tab,
        Check("Tab finishes the only match", |a| draft(a) == "ls Cargo.toml "),
        Send,
        Until("completed command ran", finished, 5_000),
        Check("completed ls block exit 0", |a| last_cmd(a, 0).is_some_and(|b| b.cmd == "ls Cargo.toml" && b.exit == Some(0))),
        Shot("01-blocks"),
        Cmd("claude"),
        Until("claude is up", |a| a.program && screen_has(a, "❯"), 20_000),
        Wait(1500),
        Check("keys go straight to claude", |a| a.direct),
        EditorFocused("input box let go of the keyboard", false),
        Shot("02-claude"),
        Keys("hello brave world"),
        Until("typed text shows in claude's box", |a| screen_has(a, "hello brave world"), 3_000),
        ClickText("brave"),
        Wait(600),
        Keys("very "),
        Until("click moved claude's cursor", |a| screen_has(a, "hello very brave world"), 3_000),
        SelectText("brave"),
        Keys("B"),
        Until("typing replaced the selection", |a| screen_has(a, "hello very B world"), 3_000),
        Paste(" pasted"),
        Until("paste lands at the cursor", |a| screen_has(a, "very B pasted world"), 3_000),
        ClickAfter("world"),
        ShiftEnter,
        Keys("second line here"),
        Until("Shift-Enter makes a second line", |a| screen_has(a, "second line here") && screen_has(a, "very B pasted world"), 3_000),
        ClickText("pasted"),
        Keys("X"),
        Until("click on the line above moves the cursor up", |a| screen_has(a, "B Xpasted world"), 5_000),
        SelectSpan("world", "second"),
        Keys("Z"),
        Until("selection across two lines is replaced", |a| screen_has(a, "Xpasted Z line here"), 6_000),
        Shot("03-edited"),
        ClickBox,
        Check("clicking the box switches to drafting", |a| !a.direct),
        EditorFocused("box has the keyboard after a click", true),
        Shot("04-box"),
        ClickText("hello"),
        Check("clicking claude's line switches back", |a| a.direct),
        EditorFocused("box let go after clicking claude", false),
        CtrlC,
        Wait(400),
        CtrlC,
        Wait(400),
        CtrlC,
        Until("claude exited", |a| !a.program && a.at_prompt(), 10_000),
        Check("back at the prompt the box drafts", |a| !a.direct),
        EditorFocused("box has the keyboard again", true),
        Check("claude block finished", |a| last_cmd(a, 0).is_some_and(|b| b.cmd == "claude" && !b.running())),
        Shot("05-after"),
        Cmd("vim -u NONE -i NONE -N -n"),
        Until("vim takes the whole window", |a| a.term.alt_screen, 5_000),
        Keys("ihello vim"),
        Until("keys reach vim", |a| screen_has(a, "hello vim"), 3_000),
        Shot("06-vim"),
        Esc,
        Keys(":q!"),
        Enter,
        Until("vim exited back to blocks", |a| !a.term.alt_screen && finished(a), 5_000),
        EditorFocused("box has the keyboard after vim", true),
        Shot("07-after-vim"),
    ]
}

fn draft(a: &App) -> String {
    a.editor.text().trim_end_matches('\n').to_string()
}

fn finished(a: &App) -> bool {
    a.at_prompt() && a.term.blocks.last().is_some_and(|b| !b.running())
}

fn last_cmd(a: &App, back: usize) -> Option<&crate::term::Block> {
    a.term.blocks.iter().rev().filter(|b| !b.cmd.is_empty()).nth(back)
}

/// (absolute line, column) of `needle` on the live screen, bottom row first.
fn find(a: &App, needle: &str) -> Option<(usize, usize)> {
    let n: Vec<char> = needle.chars().collect();
    (0..a.term.rows).rev().find_map(|r| {
        let l = a.term.abs(r);
        let cells: Vec<char> = a.term.line(l)?.iter().map(|c| if c.ch == '\u{a0}' { ' ' } else { c.ch }).collect();
        cells.windows(n.len()).position(|w| w == n.as_slice()).map(|c| (l, c))
    })
}

fn screen_has(a: &App, s: &str) -> bool {
    find(a, s).is_some()
}

pub struct Run {
    dir: PathBuf,
    steps: Vec<Step>,
    i: usize,
    since: Instant,
    fails: usize,
}

impl Run {
    pub fn from_env() -> Option<Run> {
        let dir = PathBuf::from(std::env::var_os("NIB_QA")?);
        std::fs::create_dir_all(&dir).ok()?;
        let _ = std::fs::remove_file(dir.join("qa.log"));
        Some(Run { dir, steps: script(), i: 0, since: Instant::now(), fails: 0 })
    }

    pub fn log(&mut self, pass: bool, what: &str) {
        self.fails += usize::from(!pass);
        let line = format!("{} {what}", if pass { "PASS" } else { "FAIL" });
        eprintln!("qa: {line}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("qa.log")) {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn save(&mut self, name: &str, s: &window::Screenshot) {
        let path = self.dir.join(format!("{name}-{}x{}.rgba", s.size.width, s.size.height));
        let ok = std::fs::write(&path, &s.rgba).is_ok();
        self.log(ok, &format!("snapshot {}", path.display()));
    }

    fn advance(&mut self) {
        self.i += 1;
        self.since = Instant::now();
    }
}

fn after(ms: u64) -> Task<Msg> {
    Task::perform(tokio::time::sleep(Duration::from_millis(ms)), |_| Msg::QaNext)
}

fn stray(key: Key, ctrl: bool, text: Option<String>) -> Msg {
    Msg::Stray { key, shift: false, ctrl, cmd: false, text }
}

/// Runs steps until one needs to wait; returns what to do next.
pub fn next(app: &mut App) -> Task<Msg> {
    loop {
        let Some(run) = app.qa.as_mut() else { return Task::none() };
        let Some(&step) = run.steps.get(run.i) else {
            let total = run.steps.len();
            let fails = run.fails;
            run.log(fails == 0, &format!("done: {fails} failed of {total} steps"));
            return iced::exit();
        };
        let waited = run.since.elapsed().as_millis() as u64;
        match step {
            Wait(ms) => {
                run.advance();
                return after(ms);
            }
            Until(label, cond, timeout) => {
                if cond(app) {
                    let run = app.qa.as_mut().unwrap();
                    run.log(true, label);
                    run.advance();
                } else if waited > timeout {
                    let run = app.qa.as_mut().unwrap();
                    run.log(false, &format!("{label} (timed out after {timeout}ms)"));
                    run.advance();
                } else {
                    return after(100);
                }
            }
            Check(label, cond) => {
                let ok = cond(app);
                let run = app.qa.as_mut().unwrap();
                run.log(ok, label);
                run.advance();
            }
            EditorFocused(label, want) => {
                run.advance();
                return operation::is_focused(Id::new(EDITOR)).map(move |got| Msg::QaFocus(label, want, got));
            }
            Shot(name) => {
                run.advance();
                return window::latest()
                    .and_then(window::screenshot)
                    .map(move |s| Msg::QaShot(name, s));
            }
            Cmd(s) => {
                run.advance();
                for c in s.chars() {
                    let _ = app.update(Msg::Edit(EdAction::Edit(Edit::Insert(c))));
                }
                return Task::batch([app.update(Msg::Route(KeyAction::Send)), after(300)]);
            }
            Type(s) => {
                run.advance();
                for c in s.chars() {
                    let _ = app.update(Msg::Edit(EdAction::Edit(Edit::Insert(c))));
                }
            }
            Tab | Send => {
                run.advance();
                let a = if matches!(step, Tab) { KeyAction::Complete } else { KeyAction::Send };
                return Task::batch([app.update(Msg::Route(a)), after(200)]);
            }
            Esc | Enter => {
                run.advance();
                let key = if matches!(step, Esc) { Key::Escape } else { Key::Ret };
                return Task::batch([app.update(stray(key, false, None)), after(200)]);
            }
            Keys(s) => {
                run.advance();
                let tasks: Vec<_> = s.chars().map(|c| app.update(stray(Key::Char(c), false, Some(c.to_string())))).collect();
                return Task::batch(tasks.into_iter().chain([after(200)]));
            }
            CtrlC => {
                run.advance();
                return Task::batch([app.update(stray(Key::Char('c'), true, Some("c".into()))), after(100)]);
            }
            ClickText(t) | SelectText(t) | ClickAfter(t) => {
                run.advance();
                let at = |(l, c): (usize, usize)| if matches!(step, ClickAfter(_)) { (l, c + t.chars().count()) } else { (l, c) };
                let Some(p) = find(app, t).map(at) else {
                    app.qa.as_mut().unwrap().log(false, &format!("text {t:?} not on screen to click"));
                    continue;
                };
                let _ = app.update(Msg::SelStart(p));
                if matches!(step, SelectText(_)) {
                    let _ = app.update(Msg::SelTo((p.0, p.1 + t.chars().count() - 1)));
                }
                return Task::batch([app.update(Msg::Release), after(300)]);
            }
            SelectSpan(from, to) => {
                run.advance();
                let (Some(a), Some(b)) = (find(app, from), find(app, to)) else {
                    app.qa.as_mut().unwrap().log(false, &format!("{from:?}..{to:?} not on screen to select"));
                    continue;
                };
                let _ = app.update(Msg::SelStart(a));
                let _ = app.update(Msg::SelTo((b.0, b.1 + to.chars().count() - 1)));
                return Task::batch([app.update(Msg::Release), after(300)]);
            }
            ShiftEnter => {
                run.advance();
                let enter = Msg::Stray { key: Key::Ret, shift: true, ctrl: false, cmd: false, text: Some("\r".into()) };
                return Task::batch([app.update(enter), after(200)]);
            }
            ClickBox => {
                run.advance();
                let _ = app.update(Msg::Edit(EdAction::Click(Point::new(4.0, 4.0))));
                return Task::batch([operation::focus(Id::new(EDITOR)), after(200)]);
            }
            Paste(s) => {
                run.advance();
                return Task::batch([app.update(Msg::PasteRaw(Some(s.to_string()))), after(200)]);
            }
        }
    }
}
