# Roadmap

What comes next, with rough hours for each part. About 31 hours in all.

## Shells (about 3 hours)

- Blocks in fish, through a conf.d file that hooks the fish_preexec and fish_postexec events.
- On macOS's bash 3.2, keep a DEBUG trap from your config working next to the one Nib adds. Today Nib's replaces it.

## Completion (about 4 hours)

- Completion that knows the command: git subcommands and branches, npm and cargo scripts, make targets, ssh hosts from ~/.ssh/config.
- Tab in the middle of a draft, not only at the end.

## Blocks (about 5 hours)

- Collapse long output, and move between blocks from the keyboard.
- Find text across every block with Cmd+F.
- Save a session's blocks to a file and open it again later.

## Links (about 3 hours)

- OSC 8 hyperlinks, and URLs and file paths found in output, opened with Cmd+click.

## Remote shells (about 4 hours)

- Blocks after you ssh into another machine, by sending the hooks to the remote shell for that session.

## Terminal fixes (about 2 hours)

- Mouse reporting and focus events for programs that ask for them, like htop or vim with the mouse on.

## Window (about 6 hours)

- Tabs and split panes.
- A settings file for the font, text size, colors and key bindings.
- A .app bundle with an icon, so Nib opens from the Dock.

## Linux (about 2 hours)

- Build and run on Linux. forkpty and iced both work there; what's missing is testing it and fixing what turns up.

## Testing (about 2 hours)

- QA scripts read from files, so a new case doesn't need a rebuild.
- QA scripts for vim, less and a Python REPL.
- CI that builds and runs the tests on macOS.

## Not planned

Nib stays small. No accounts or sign in, no cloud sync, no telemetry, and no AI chat panel.
