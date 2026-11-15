//! Our PTY: forkpty a login shell, hand its output to a reader, take keystrokes and resizes.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};

#[derive(Debug)]
pub struct Pty {
    master: File,
    pub pid: libc::pid_t,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

impl Pty {
    /// `shell` must be an absolute path. The child inherits our environment (TERM etc. set in main).
    pub fn spawn(shell: &str, dir: &str, cols: u16, rows: u16) -> io::Result<Pty> {
        // Build every C string before fork: after fork only async-signal-safe calls are allowed.
        let name = shell.rsplit('/').next().unwrap_or(shell);
        let prog = CString::new(shell)?;
        let argv0 = CString::new(format!("-{name}"))?; // leading '-' = login shell, so PATH gets set up
        let dir = CString::new(dir)?;
        let argv = [argv0.as_ptr(), std::ptr::null()];
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
        Ok(Pty { master: unsafe { File::from_raw_fd(fd) }, pid })
    }

    /// A second handle on the same PTY for the reader thread.
    pub fn reader(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    pub fn write(&self, bytes: &[u8]) {
        // ponytail: blocking write on the UI thread; move to a writer thread if huge pastes stutter.
        let _ = (&self.master).write_all(bytes);
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
        let pty = Pty::spawn("/bin/zsh", "/tmp", 80, 24).unwrap();
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
}
