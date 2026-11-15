//! zsh integration: a tiny ZDOTDIR shim that loads the user's own config, then adds hooks that
//! mark each prompt and command with OSC 133 (and the folder with OSC 7) so Nib can draw blocks.

use std::path::PathBuf;

const HOOKS: &str = r#"# Nib block marks. Loaded once, before your own .zshrc.
autoload -Uz add-zsh-hook
_nib_precmd() {
  local ret=$?
  # Blocks track lines that end without a newline, so zsh's inverse "%" mark would only be noise in them.
  # Set here, not once, so a .zshrc loaded after us can't turn it back on.
  setopt no_prompt_sp
  [[ -n $_nib_running ]] && printf '\e]133;D;%s\a' "$ret"
  _nib_running=
  printf '\e]7;file://%s%s\a\e]133;A\a' "$HOST" "$PWD"
}
_nib_preexec() {
  _nib_running=1
  printf '\e]133;C\a'
}
add-zsh-hook precmd _nib_precmd
add-zsh-hook preexec _nib_preexec
"#;

/// Our .zshenv: hand ZDOTDIR back to the user straight away, so their .zshenv/.zprofile/.zshrc
/// load exactly as in any other terminal (and child shells never see this directory).
fn zshenv(dir: &str) -> String {
    format!(
        r#"if [[ -n $NIB_USER_ZDOTDIR ]]; then ZDOTDIR=$NIB_USER_ZDOTDIR; else unset ZDOTDIR; fi
unset NIB_USER_ZDOTDIR
[[ -f "${{ZDOTDIR:-$HOME}}/.zshenv" ]] && source "${{ZDOTDIR:-$HOME}}/.zshenv"
[[ -o interactive ]] && source "{dir}/nib.zsh"
"#
    )
}

/// Writes the shim and points the next zsh we start at it. Other shells get no blocks (plain terminal).
pub fn install(shell: &str) -> std::io::Result<Option<PathBuf>> {
    if !shell.ends_with("/zsh") {
        return Ok(None);
    }
    let dir = std::env::temp_dir().join("nib-zsh");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("nib.zsh"), HOOKS)?;
    std::fs::write(dir.join(".zshenv"), zshenv(&dir.to_string_lossy()))?;
    if let Ok(user) = std::env::var("ZDOTDIR") {
        std::env::set_var("NIB_USER_ZDOTDIR", user);
    }
    std::env::set_var("ZDOTDIR", &dir);
    Ok(Some(dir))
}

#[cfg(test)]
mod tests {
    use crate::pty::Pty;
    use crate::term::Term;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    #[test]
    fn real_zsh_produces_blocks_with_exit_codes() {
        let _env = crate::pty::SHELL_ENV.lock().unwrap_or_else(|e| e.into_inner());
        // Stand-in for the user's config dir: empty, so their .zshrc can't slow or change the test.
        let user = std::env::temp_dir().join("nib-test-user-zdotdir");
        std::fs::create_dir_all(&user).unwrap();
        std::env::set_var("ZDOTDIR", &user);
        std::env::set_var("TERM", "xterm-256color");
        super::install("/bin/zsh").unwrap();
        let pty = Pty::spawn("/bin/zsh", "/tmp", 80, 24).unwrap();
        let mut r = pty.reader().unwrap();
        unsafe { libc::fcntl(r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let mut t = Term::new(80, 24);
        let mut pump = |t: &mut Term, done: &dyn Fn(&Term) -> bool| {
            let end = Instant::now() + Duration::from_secs(5);
            let mut buf = [0u8; 8192];
            while !done(t) && Instant::now() < end {
                match r.read(&mut buf) {
                    Ok(n @ 1..) => t.feed(&buf[..n]),
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        };
        pump(&mut t, &|t| t.integrated);
        assert!(t.integrated, "no prompt mark from zsh");
        assert!(t.cwd.ends_with("/tmp"), "cwd was {:?}", t.cwd);
        // Regression: ISSUE-005 — zsh's "%" no-newline mark landed inside the block of output without a final newline
        // Found by /qa on 2026-11-15
        // Report: .gstack/qa-reports/run-20261115T002112Z/qa-report-termpad-2026-11-15.md
        for cmd in ["echo hello-block", "false", "printf abc"] {
            t.begin_block(cmd, "");
            pty.write(format!("{cmd}\r").as_bytes());
            let n = t.blocks.len();
            pump(&mut t, &|t| t.blocks.len() == n && !t.blocks[n - 1].running());
        }
        let text = |t: &Term, b: &crate::term::Block| -> Vec<String> {
            (b.start.unwrap()..b.end.unwrap())
                .map(|l| t.line(l).unwrap().iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
                .collect()
        };
        let last = t.blocks.last().unwrap();
        assert_eq!(text(&t, last), ["abc"], "no-newline output keeps no % mark");
        let b = &t.blocks[t.blocks.len() - 3..];
        assert_eq!((b[0].cmd.as_str(), b[0].exit), ("echo hello-block", Some(0)));
        let out: Vec<String> = (b[0].start.unwrap()..b[0].end.unwrap())
            .map(|l| t.line(l).unwrap().iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(out, ["hello-block"]);
        assert_eq!((b[1].cmd.as_str(), b[1].exit), ("false", Some(1)));
        // The user's ZDOTDIR is handed back, so shells started from this one never see the shim.
        std::env::remove_var("ZDOTDIR");
    }
}

#[cfg(test)]
mod real_config {
    use crate::pty::Pty;
    use crate::term::Term;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    /// Run by hand (`cargo test -- --ignored`): your own ~/.zshrc must not break the block marks.
    #[test]
    #[ignore]
    fn blocks_survive_the_users_own_zshrc() {
        let _env = crate::pty::SHELL_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ZDOTDIR");
        std::env::set_var("TERM", "xterm-256color");
        super::install("/bin/zsh").unwrap();
        let pty = Pty::spawn("/bin/zsh", "/tmp", 100, 30).unwrap();
        let mut r = pty.reader().unwrap();
        unsafe { libc::fcntl(r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let mut t = Term::new(100, 30);
        let mut pump = |t: &mut Term, secs: u64, done: &dyn Fn(&Term) -> bool| {
            let end = Instant::now() + Duration::from_secs(secs);
            let mut buf = [0u8; 8192];
            while !done(t) && Instant::now() < end {
                match r.read(&mut buf) {
                    Ok(n @ 1..) => t.feed(&buf[..n]),
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        };
        pump(&mut t, 15, &|t| t.integrated);
        assert!(t.integrated, "no prompt mark: something in ~/.zshrc drops the hooks");
        pump(&mut t, 2, &|_| false); // let a slow prompt finish drawing
        t.begin_block("ls /nonexistent-nib", "");
        pty.write(b"ls /nonexistent-nib\r");
        let n = t.blocks.len();
        pump(&mut t, 10, &|t| !t.blocks[n - 1].running());
        let b = &t.blocks[n - 1];
        eprintln!("blocks: {:?}", t.blocks.iter().map(|b| (&b.cmd, b.start, b.end, b.exit)).collect::<Vec<_>>());
        assert_ne!(b.exit, Some(0), "ls of a missing path should fail");
        assert!(b.exit.is_some(), "no finish mark");
        std::env::remove_var("ZDOTDIR");
    }
}

#[cfg(test)]
mod claude_probe {
    use crate::pty::Pty;
    use crate::term::Term;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    /// Diagnostic, run by hand: start `claude` exactly as the app does and dump what it asks of the terminal.
    #[test]
    #[ignore]
    fn claude_probe() {
        let _env = crate::pty::SHELL_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ZDOTDIR");
        std::env::set_var("TERM", "xterm-256color");
        std::env::set_var("COLORTERM", "truecolor");
        std::env::set_var("TERM_PROGRAM", "Nib");
        std::env::remove_var("CLAUDECODE");
        std::env::remove_var("CLAUDE_CODE_CHILD_SESSION");
        super::install("/bin/zsh").unwrap();
        let dir = env!("CARGO_MANIFEST_DIR");
        let pty = Pty::spawn("/bin/zsh", dir, 110, 32).unwrap();
        let mut r = pty.reader().unwrap();
        unsafe { libc::fcntl(r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let mut t = Term::new(110, 32);
        let mut raw = Vec::new();
        let mut pump = |t: &mut Term, raw: &mut Vec<u8>, tenths: u64| {
            let end = Instant::now() + Duration::from_millis(tenths * 100);
            let mut buf = [0u8; 8192];
            while Instant::now() < end {
                match r.read(&mut buf) {
                    Ok(n @ 1..) => {
                        raw.extend_from_slice(&buf[..n]);
                        t.feed(&buf[..n]);
                        let rep = std::mem::take(&mut t.replies);
                        if !rep.is_empty() {
                            eprintln!("REPLY {:?}", String::from_utf8_lossy(&rep));
                            pty.write(&rep);
                        }
                    }
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        };
        pump(&mut t, &mut raw, 60);
        raw.clear();
        t.begin_block("claude", "");
        pty.write(b"claude\r");
        pump(&mut t, &mut raw, 100);
        let dump = |t: &Term, label: &str| {
            eprintln!("=== {label}: alt={} paste={} app_cursor={} cursor_visible={} at_prompt={} cursor=({},{})",
                t.alt_screen, t.bracketed_paste, t.app_cursor, t.cursor_visible, pty.at_shell_prompt(), t.row, t.col);
            for i in 0..t.rows { let s = t.row_text(i); if !s.trim().is_empty() { eprintln!("{i:2}|{s}"); } }
        };
        dump(&t, "after start");
        let esc = String::from_utf8_lossy(&raw).to_string();
        let mut seen: Vec<String> = Vec::new();
        for (i, _) in esc.match_indices("\x1b[") {
            let tail: String = esc[i + 2..].chars().take_while(|c| !c.is_ascii_alphabetic() && *c != '~').take(20).collect();
            let fin = esc[i + 2 + tail.len()..].chars().next().unwrap_or(' ');
            let s = format!("CSI {tail}{fin}");
            if (tail.starts_with('?') || tail.starts_with('>') || tail.starts_with('<') || tail.starts_with('=') || fin == 'c' || fin == 'n' || fin == 'q' || fin == 'p') && !seen.contains(&s) { seen.push(s); }
        }
        for (i, _) in esc.match_indices("\x1b]") {
            let s: String = esc[i + 2..].chars().take_while(|c| *c != '\x07' && *c != '\x1b').take(30).collect();
            let s = format!("OSC {s}");
            if !seen.contains(&s) { seen.push(s); }
        }
        eprintln!("MODES {seen:?}");
        let (paste, _) = crate::keys::payload("probe text one two", t.bracketed_paste);
        pty.write(&paste);
        pump(&mut t, &mut raw, 30);
        dump(&t, "after paste");
        // Walk the cursor like the app does: step, let Claude redraw, look again.
        let walk = |t: &mut Term, raw: &mut Vec<u8>, goal: (usize, usize), erase: bool, pump: &mut dyn FnMut(&mut Term, &mut Vec<u8>, u64)| {
            for _ in 0..30 {
                let Some(keys) = crate::direct::step(t, goal, erase) else { return true };
                pty.write(&keys);
                pump(t, raw, 1);
            }
            false
        };
        let line = t.abs(t.row);
        let cells: Vec<char> = t.line(line).unwrap().iter().map(|c| c.ch).collect();
        let col = cells.windows(4).position(|w| w == ['t', 'e', 'x', 't']).unwrap();
        let goal = crate::direct::target(&t, (line, col)).unwrap();
        assert!(walk(&mut t, &mut raw, goal, false, &mut pump), "click never landed");
        pty.write(b"X");
        pump(&mut t, &mut raw, 1);
        dump(&t, "after click before 'text' + X");
        let row = t.line(line).unwrap().iter().map(|c| c.ch).collect::<String>();
        assert!(row.contains("probe Xtext one two"), "row was {row:?}");
        // Select "one" and type Y over it: walk to its end, erase back to its start, type.
        let chars: Vec<char> = t.line(line).unwrap().iter().map(|c| c.ch).collect();
        let s = chars.windows(3).position(|w| w == ['o', 'n', 'e']).unwrap();
        let end = crate::direct::target(&t, (line, s + 3)).unwrap();
        let start = crate::direct::target(&t, (line, s)).unwrap();
        assert!(walk(&mut t, &mut raw, end, false, &mut pump));
        assert!(walk(&mut t, &mut raw, start, true, &mut pump));
        pty.write(b"Y");
        pump(&mut t, &mut raw, 1);
        dump(&t, "after replacing 'one' with Y");
        let row = t.line(line).unwrap().iter().map(|c| c.ch).collect::<String>();
        assert!(row.contains("probe Xtext Y two"), "row was {row:?}");
        pty.write(b"\x03");
        pump(&mut t, &mut raw, 1);
        pty.write(b"\x03");
        pump(&mut t, &mut raw, 2);
        dump(&t, "after ctrl-c x2");
    }
}
