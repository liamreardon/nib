# Roadmap

What comes next, with rough hours for each part. About 26 hours in all.

## Shells (about 4 hours)

- Blocks in bash, with the hooks loaded through --rcfile, using PROMPT_COMMAND and a DEBUG trap.
- Blocks in fish, through a conf.d file that hooks the fish_preexec and fish_postexec events.

## Completion (about 4 hours)

- Completion that knows the command: git subcommands and branches, npm and cargo scripts, make targets, ssh hosts from ~/.ssh/config.
- Tab in the middle of a draft, not only at the end.

## Blocks (about 6 hours)

- Copy a block's command or output with one click, and rerun a command from its block.
- Collapse long output, and move between blocks from the keyboard.
- Find text across every block with Cmd+F.
- Save a session's blocks to a file and open it again later.

## Terminal fixes (about 4 hours)

- Mouse reporting and focus events for programs that ask for them, like htop or vim with the mouse on.
- The DEC line drawing characters (ESC ( 0) that older programs use for boxes.
- Combining marks, attached to the cell before them so accents draw right.
- Writes to the shell from a separate thread, so a big paste doesn't freeze the window.

## Window (about 6 hours)

- Tabs and split panes.
- A settings file for the font, text size, colors and key bindings.
- A .app bundle with an icon, so Nib opens from the Dock.

## Testing (about 2 hours)

- QA scripts read from files, so a new case doesn't need a rebuild.
- QA scripts for vim, less and a Python REPL.

## Not planned

Nib stays small. No accounts or sign in, no cloud sync, no telemetry, and no AI chat panel.
