//! Tab in the input box: finish the word before the cursor from PATH commands (first word) or from files
//! in the shell's folder. One match completes it; several extend to what they share and get listed.
//! Whatever follows the cursor stays where it is. git also gets its subcommands and aliases, and
//! branches and tags for the subcommands that take them.
// ponytail: git is the only command with its own list; add specs (npm scripts, make targets) the same way.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const BUILTINS: &[&str] = &["alias", "cd", "echo", "exit", "export", "history", "source", "type", "unset", "which"];

/// Start of the last word: after the last space that isn't escaped with a backslash.
fn word_start(s: &str) -> usize {
    let b = s.as_bytes();
    (0..b.len()).rev().find(|&i| b[i].is_ascii_whitespace() && (i == 0 || b[i - 1] != b'\\')).map_or(0, |i| i + 1)
}

fn escape(name: &str) -> String {
    name.chars().flat_map(|c| if " '\"()&;|$`\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// (replacement for the whole word, shown name, is a folder)
fn paths(word: &str, cwd: &str) -> Vec<(String, String, bool)> {
    let (dir, prefix) = word.rsplit_once('/').map_or(("", word), |(d, p)| (d, p));
    let dir_part = if word.contains('/') { format!("{dir}/") } else { String::new() };
    let unescaped = dir_part.replace('\\', "");
    let home = std::env::var("HOME").unwrap_or_default();
    let base = match unescaped.strip_prefix("~/") {
        Some(rest) => Path::new(&home).join(rest),
        None if unescaped.starts_with('/') => Path::new(&unescaped).to_path_buf(),
        None => Path::new(cwd).join(&unescaped),
    };
    let prefix = prefix.replace('\\', "");
    let Ok(entries) = std::fs::read_dir(&base) else { return vec![] };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let hidden = name.starts_with('.') && !prefix.starts_with('.');
            (!hidden && name.starts_with(&prefix)).then(|| {
                let is_dir = e.path().is_dir();
                (format!("{dir_part}{}", escape(&name)), if is_dir { format!("{name}/") } else { name }, is_dir)
            })
        })
        .collect()
}

const GIT_COMMANDS: &[&str] = &[
    "add", "bisect", "blame", "branch", "checkout", "cherry-pick", "clone", "commit", "diff", "fetch", "grep",
    "init", "log", "merge", "mv", "pull", "push", "rebase", "reflog", "remote", "reset", "restore", "revert",
    "rm", "show", "stash", "status", "switch", "tag", "worktree",
];

/// git subcommands whose arguments are usually branches or tags (files still complete too).
const GIT_TAKES_REF: &[&str] = &["branch", "checkout", "cherry-pick", "diff", "log", "merge", "rebase", "reset", "show", "switch"];

/// Lines git prints for `args` in `cwd`; nothing when git is missing or it isn't a repository.
fn git(args: &[&str], cwd: &str) -> Vec<String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Choices for the word after `git` or after one of its subcommands. None: plain completion applies.
fn git_opts(words: &[&str], word: &str, cwd: &str) -> Option<Vec<(String, String, bool)>> {
    if words.first() != Some(&"git") || word.starts_with('-') {
        return None;
    }
    let names: Vec<String> = match words {
        [_] => {
            let aliases = git(&["config", "--get-regexp", r"^alias\."], cwd);
            let aliases = aliases.iter().filter_map(|l| l.strip_prefix("alias.")?.split_whitespace().next());
            GIT_COMMANDS.iter().copied().chain(aliases).map(str::to_string).collect()
        }
        [_, sub, ..] if GIT_TAKES_REF.contains(sub) => {
            git(&["for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes", "refs/tags"], cwd)
                .into_iter()
                .filter(|r| !r.ends_with("HEAD"))
                .collect()
        }
        _ => return None,
    };
    let mut opts: Vec<_> = names.into_iter().filter(|n| n.starts_with(word)).map(|n| (escape(&n), n, false)).collect();
    opts.sort();
    opts.dedup();
    if words.len() > 1 {
        opts.extend(paths(word, cwd));
    }
    Some(opts)
}

/// Words of the command the cursor is in, up to the word being completed.
fn command_words(before: &str) -> Vec<&str> {
    let start = before.rfind(['|', ';', '&', '(', '\n']).map_or(0, |i| i + 1);
    before[start..].split_whitespace().collect()
}

fn commands(prefix: &str, path_var: &str) -> Vec<(String, String, bool)> {
    let mut out: Vec<String> = BUILTINS.iter().filter(|b| b.starts_with(prefix)).map(|b| b.to_string()).collect();
    for dir in path_var.split(':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let exec = e.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
            if exec && name.starts_with(prefix) {
                out.push(name);
            }
        }
    }
    out.sort();
    out.dedup();
    out.into_iter().map(|n| (n.clone(), n, false)).collect()
}

/// The draft after Tab, and the choices to show when there's more than one. None: nothing matches.
pub fn complete(draft: &str, cwd: &str, path_var: &str) -> Option<(String, Vec<String>)> {
    let start = word_start(draft);
    let (before, word) = draft.split_at(start);
    let prev = before.trim_end_matches([' ', '\t']);
    let first = prev.is_empty() || prev.ends_with(['|', ';', '&', '(', '\n']);
    let mut opts = match git_opts(&command_words(before), word, cwd) {
        Some(git) => git,
        None if first && !word.contains('/') => commands(word, path_var),
        None => paths(word, cwd),
    };
    opts.sort();
    let (first_opt, _, is_dir) = opts.first()?.clone();
    if opts.len() == 1 {
        return Some((format!("{before}{first_opt}{}", if is_dir { "/" } else { " " }), vec![]));
    }
    let common = opts.iter().fold(first_opt, |acc, (o, ..)| {
        acc.chars().zip(o.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
    });
    let shown = opts.into_iter().map(|(_, name, _)| name).collect();
    Some((format!("{before}{}", if common.len() > word.len() { &common } else { word }), shown))
}

/// Tab with the cursor at byte `at`: completes the word ending there and keeps the rest of the draft.
/// Returns the new draft, where the cursor goes (a byte offset) and the choices to show.
pub fn complete_at(draft: &str, at: usize, cwd: &str, path_var: &str) -> Option<(String, usize, Vec<String>)> {
    let at = (0..=at.min(draft.len())).rev().find(|&i| draft.is_char_boundary(i)).unwrap_or(0);
    let (before, after) = draft.split_at(at);
    let (mut new, shown) = complete(before, cwd, path_var)?;
    if new.ends_with(' ') && after.starts_with([' ', '\n']) {
        new.pop(); // don't double the space that's already there
    }
    let cursor = new.len();
    Some((new + after, cursor, shown))
}

#[cfg(test)]
mod tests {
    use super::{complete, complete_at};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn completes_files_folders_and_commands() {
        let root = std::env::temp_dir().join("nib-test-complete");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("alpine")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        for f in ["alpha.txt", "beta", "my file", ".hidden"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        std::fs::write(root.join("bin/zzfoo"), "").unwrap();
        std::fs::set_permissions(root.join("bin/zzfoo"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let cwd = root.to_string_lossy().to_string();
        let path = root.join("bin").to_string_lossy().to_string();
        let c = |d: &str| complete(d, &cwd, &path);
        assert_eq!(c("cat al"), Some(("cat alp".into(), vec!["alpha.txt".into(), "alpine/".into()])));
        assert_eq!(c("cat alpi"), Some(("cat alpine/".into(), vec![])), "one folder: add the slash");
        assert_eq!(c("cat be"), Some(("cat beta ".into(), vec![])), "one file: add a space");
        assert_eq!(c("cat my"), Some(("cat my\\ file ".into(), vec![])), "spaces get escaped");
        assert_eq!(c("cat alpine/"), None, "empty folder");
        assert_eq!(c("zzf"), Some(("zzfoo ".into(), vec![])), "first word: commands from PATH");
        assert_eq!(c("ls | zzf"), Some(("ls | zzfoo ".into(), vec![])), "after a pipe: commands again");
        assert_eq!(c("cat nothing"), None);
        assert!(c("cat ").unwrap().1.iter().all(|n| !n.starts_with('.')), "dotfiles stay hidden");
        // Mid-draft: the word before the cursor completes, the rest of the draft stays after it.
        let at = |d: &str, i: usize| complete_at(d, i, &cwd, &path);
        assert_eq!(at("cat be | wc -l", 6), Some(("cat beta | wc -l".into(), 8, vec![])));
        assert_eq!(at("cat be", 6), Some(("cat beta ".into(), 9, vec![])), "at the end, as before");
        assert_eq!(at("cat al && ls", 6).map(|(s, i, _)| (s, i)), Some(("cat alp && ls".into(), 7)));
        assert_eq!(at("ls\nzzf", 6).map(|(s, _, _)| s), Some("ls\nzzfoo ".into()), "a new line starts a new command");
        assert_eq!(at("cat be", 99).map(|(s, _, _)| s), Some("cat beta ".into()), "a cursor past the end is clamped");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn completes_git_subcommands_aliases_and_branches() {
        let root = std::env::temp_dir().join("nib-test-git-complete");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").args(args).current_dir(&root).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--no-verify", "--allow-empty", "-m", "x"]);
        git(&["branch", "feature-login"]);
        git(&["branch", "feature-logout"]);
        git(&["tag", "v1.0"]);
        git(&["config", "alias.zzco", "checkout"]);
        std::fs::write(root.join("featured.txt"), "").unwrap();
        let cwd = root.to_string_lossy().to_string();
        let c = |d: &str| complete(d, &cwd, "");
        assert_eq!(c("git chec"), Some(("git checkout ".into(), vec![])), "subcommand");
        assert_eq!(c("git zz"), Some(("git zzco ".into(), vec![])), "alias from git config");
        assert_eq!(c("ls && git sw"), Some(("ls && git switch ".into(), vec![])), "git after &&");
        assert_eq!(
            c("git switch feature-log"),
            Some(("git switch feature-log".into(), vec!["feature-login".into(), "feature-logout".into()])),
            "branches"
        );
        assert_eq!(c("git log v1"), Some(("git log v1.0 ".into(), vec![])), "tags");
        let both = c("git checkout feat").unwrap();
        assert_eq!(both.0, "git checkout feature", "branches and files share the prefix");
        assert!(both.1.contains(&"featured.txt".to_string()), "files still complete: {:?}", both.1);
        assert_eq!(c("git add feat"), Some(("git add featured.txt ".into(), vec![])), "add takes files, not branches");
        assert_eq!(c("git checkout -"), None, "flags are left alone");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
