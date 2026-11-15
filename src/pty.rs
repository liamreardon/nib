//! Our PTY: forkpty a login shell, hand its output to a reader, take keystrokes and resizes.
//! Keystrokes and pastes go through a writer thread, so a full PTY never stalls the window.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::mpsc;

#[derive(Debug)]
pub struct Pty {
    master: File,
    writes: mpsc::Sender<Vec<u8>>, // to the writer thread, in order
    pub pid: libc::pid_t,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

impl Pty {
    /// `prog` must be an absolute path; `argv` comes from `shell::argv`. The child inherits our
    /// environment (TERM etc. set in main).
    pub fn spawn(prog: &str, argv: &[String], dir: &str, cols: u16, rows: u16) -> io::Result<Pty> {
        // Build every C string before fork: after fork only async-signal-safe calls are allowed.
        let prog = CString::new(prog)?;
        let args = argv.iter().map(|a| CString::new(a.as_str())).collect::<Result<Vec<_>, _>>()?;
        let dir = CString::new(dir)?;
        let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut ws = winsize(cols, rows);
        let mut fd = -1;
        let pid = unsafe { libc::forkpty(&mut fd, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                libc::chdir(dir.as_ptr());
                libc::execv(prog.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }
        let master = unsafe { File::from_raw_fd(fd) };
        let mut out = master.try_clone()?;
        let (writes, queue) = mpsc::channel::<Vec<u8>>();
        // A program that stops reading (busy, or paused) fills the PTY and blocks this thread, not the
        // UI. It ends when the Pty is dropped and the queue closes, or when the shell is gone.
        std::thread::spawn(move || {
            for bytes in queue {
                if out.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        Ok(Pty { master, writes, pid })
    }

    /// A second handle on the same PTY for the reader thread.
    pub fn reader(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    pub fn write(&self, bytes: &[u8]) {
        let _ = self.writes.send(bytes.to_vec());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let ws = winsize(cols, rows);
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }

    /// True when the shell itself is in the foreground (no claude/python/vim running in front of it).
    pub fn at_shell_prompt(&self) -> bool {
        unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) == self.pid }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe { libc::kill(self.pid, libc::SIGHUP) };
    }
}

/// Tests that start real shells change process-wide env vars (ZDOTDIR), so they take turns.
#[cfg(test)]
pub(crate) static SHELL_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Term;
    use std::io::Read;
    use std::time::{Duration, Instant};

    /// Reads until `want` shows up on the screen or 5s pass.
    fn wait_for(r: &mut File, t: &mut Term, want: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 4096];
        unsafe { libc::fcntl(r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        while Instant::now() < end {
            match r.read(&mut buf) {
                Ok(n) if n > 0 => t.feed(&buf[..n]),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
            if (0..t.rows).any(|i| t.row_text(i).contains(want)) {
                return true;
            }
        }
        false
    }

    #[test]
    fn real_zsh_runs_a_pasted_command_and_reports_foreground() {
        let _env = super::SHELL_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("TERM", "xterm-256color");
        // Empty ZDOTDIR: skip the user's ~/.zshrc so its startup output and hooks don't affect the test.
        let empty = std::env::temp_dir().join("nib-test-zdotdir");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("ZDOTDIR", &empty);
        let pty = Pty::spawn("/bin/zsh", &crate::shell::argv("/bin/zsh"), "/tmp", 80, 24).unwrap();
        let mut r = pty.reader().unwrap();
        let mut t = Term::new(80, 24);
        // The quotes split the marker so the terminal's echo of this line can't match it.
        pty.write(b"PS1='REA''DY> '\r");
        assert!(wait_for(&mut r, &mut t, "READY>"), "no prompt");
        assert!(pty.at_shell_prompt());
        let (paste, enter) = crate::keys::payload("echo $((6*7))xyz", t.bracketed_paste);
        pty.write(&paste);
        pty.write(&enter);
        assert!(wait_for(&mut r, &mut t, "42xyz"), "command output missing");
                pty.write(b"sleep 5\r");
        let end = Instant::now() + Duration::from_secs(3);
        // Keep draining output: zsh waits for its output to be read before it starts the command.
        let mut buf = [0u8; 4096];
        while pty.at_shell_prompt() && Instant::now() < end {
            if let Ok(n @ 1..) = r.read(&mut buf) {
                t.feed(&buf[..n]);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!pty.at_shell_prompt(), "sleep should own the foreground");
    }

    #[test]
    fn a_big_paste_never_blocks_the_caller() {
        let _env = super::SHELL_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let empty = std::env::temp_dir().join("nib-test-zdotdir");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("ZDOTDIR", &empty);
        let pty = std::sync::Arc::new(Pty::spawn("/bin/zsh", &crate::shell::argv("/bin/zsh"), "/tmp", 80, 24).unwrap());
        let (mut r, mut t) = (pty.reader().unwrap(), Term::new(80, 24));
        pty.write(b"PS1='REA''DY> '\r");
        assert!(wait_for(&mut r, &mut t, "READY>"), "no prompt");
        // `sleep` owns the terminal and never reads, so the pasted lines pile up until the PTY is full.
        pty.write(b"sleep 30\r");
        let end = Instant::now() + Duration::from_secs(3);
        let mut buf = [0u8; 4096];
        while pty.at_shell_prompt() && Instant::now() < end {
            let _ = r.read(&mut buf); // zsh starts the command only once its echo has been read
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!pty.at_shell_prompt(), "sleep should own the foreground");
        // wait_for made the shared file description non-blocking; the app's writes are blocking.
        unsafe { libc::fcntl(r.as_raw_fd(), libc::F_SETFL, 0) };
        let (done, wait) = std::sync::mpsc::channel();
        let p = pty.clone();
        std::thread::spawn(move || {
            p.write(&b"a line of pasted text\n".repeat(50_000));
            let _ = done.send(());
        });
        assert!(wait.recv_timeout(Duration::from_secs(2)).is_ok(), "write blocked the caller");
    }
}
