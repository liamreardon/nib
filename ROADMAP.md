# Roadmap

What comes next, with rough hours for each part. About 28 hours in all.

## Shells (about 3 hours)

- Blocks in fish, through a conf.d file that hooks the fish_preexec and fish_postexec events. [#11](https://github.com/liamreardon/nib/issues/11)
- On macOS's bash 3.2, keep a DEBUG trap from your config working next to the one Nib adds. Today Nib's replaces it. [#12](https://github.com/liamreardon/nib/issues/12)

## Completion (about 3 hours)

- Completion that knows more commands: npm and cargo scripts, make targets, ssh hosts from ~/.ssh/config, and git's flags. [#13](https://github.com/liamreardon/nib/issues/13)

## Blocks (about 5 hours)

- Collapse long output, and move between blocks from the keyboard. [#14](https://github.com/liamreardon/nib/issues/14)
- Find text across every block with Cmd+F. [#15](https://github.com/liamreardon/nib/issues/15)
- Save a session's blocks to a file and open it again later. [#16](https://github.com/liamreardon/nib/issues/16)

## Links (about 3 hours)

- OSC 8 hyperlinks, and URLs and file paths found in output, opened with Cmd+click. [#17](https://github.com/liamreardon/nib/issues/17)

## Remote shells (about 4 hours)

- Blocks after you ssh into another machine, by sending the hooks to the remote shell for that session. [#18](https://github.com/liamreardon/nib/issues/18)

## Window (about 6 hours)

- Tabs and split panes. [#19](https://github.com/liamreardon/nib/issues/19)
- A settings file for the font, text size, colors and key bindings. [#20](https://github.com/liamreardon/nib/issues/20)
- A .app bundle with an icon, so Nib opens from the Dock. [#21](https://github.com/liamreardon/nib/issues/21)

## Linux (about 2 hours)

- Build and run on Linux. forkpty and iced both work there; what's missing is testing it and fixing what turns up. [#22](https://github.com/liamreardon/nib/issues/22)

## Testing (about 2 hours)

- QA scripts read from files, so a new case doesn't need a rebuild. [#23](https://github.com/liamreardon/nib/issues/23)
- QA scripts for vim, less and a Python REPL.
- CI on Linux once Nib builds there.

## Not planned

Nib stays small. No accounts or sign in, no cloud sync, no telemetry, and no AI chat panel.
