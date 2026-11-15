//! Tab in the input box: finish the word before the cursor from PATH commands (first word) or from files
//! in the shell's folder. One match completes it; several extend to what they share and get listed.
// ponytail: no per-command knowledge (git branches, flags); add specs when plain paths stop being enough.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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
    let first = before.trim_end().is_empty() || before.trim_end().ends_with(['|', ';', '&', '(']);
    let mut opts = if first && !word.contains('/') { commands(word, path_var) } else { paths(word, cwd) };
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

#[cfg(test)]
mod tests {
    use super::complete;
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
        std::fs::remove_dir_all(&root).unwrap();
    }
}
