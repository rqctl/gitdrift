# gitdrift

Find the git repositories that have drifted — unpushed, unpulled, dirty,
stashed, conflicted — across a tree of hundreds of them.

## Install

Needs Rust 1.85 or newer and the `git` binary on `PATH` — gitdrift reads with
`gix`, but every action it takes shells out to `git` so your SSH keys,
credential helpers and `insteadOf` rules keep working.

```sh
git clone <this repo> && cd gitdrift
cargo install --path .
```

`cargo install` puts the binary in `~/.cargo/bin`. Re-run it with `--force`
after pulling changes.

## Use

```sh
gitdrift                 # TUI over the configured roots
gitdrift ~/gitlab        # TUI over one path
gitdrift --plain         # aligned table, then exit
gitdrift --json          # JSON, then exit
gitdrift --drifted-only  # hide clean, in-sync repositories
gitdrift --color=never   # or set NO_COLOR
gitdrift --config PATH   # use a different config file
```

## Keys

Press `?` once the TUI is open for the full, sectioned list of keybindings.

`Space` marks rows; `f`, `p` and `Shift+p` then act on every marked
repository instead of the one under the cursor. `Shift+v` marks everything between the last
mark and the cursor, and `a` marks the whole visible list — or clears the
marks, if there are any. Marks survive a rescan, and only ever apply to
repositories still on screen: narrowing by namespace or filter narrows the
action too, and gitdrift says so rather than quietly doing nothing.

`n` opens a picker listing every top-level namespace (`idp`, `mkp`, `aws`, …)
with its repository count, plus "all". Pick one with `↑`/`↓` and `Enter`; it
beats scrolling a few hundred rows. `o` changes the order: drift (the
default), name, most recently committed, least recently fetched. The list
pane's own title bar shows both.

`b` opens a picker of the repository's local branches. `Enter` switches to
the highlighted one; `d` diffs it against the default branch in your pager
*without* checking it out — the popover stays open with the same branch
highlighted once the pager exits, so you can diff, back out, and switch or
delete right after without reopening it; `x` deletes it (asks first).

The header at the top of the screen carries a colour-coded tally of what's
drifted across the visible repositories (e.g. `↑3 ⊘2`, or `✔ all clean`),
whatever's currently happening — a running job's spinner and progress, a
toast, or the filter you're typing — and the keybinding hints.

`Esc` closes the filter or an open pane first, and quits only when there is
nothing left to close. The help and problems panes scroll with `↑`/`↓`,
`PgUp`/`PgDn` or the wheel when they outgrow the terminal.

## Destroying work

`Shift+p` throws work away, so it needs the shift key and shows you what you
are about to lose before anything happens:

- `Shift+p` runs `git fetch --prune --prune-tags --force`, then `git branch -D`
  on every local branch whose upstream is gone, across as many repositories at
  once as `concurrency` allows. The checked-out branch and any branch checked
  out in another worktree are left alone.

This is not undoable. Press `y` to go through with it; any other key cancels.

Dropping a stash with `x` (from the stash pane) asks the same way, for the
same reason — and so does deleting a branch with `x` from the branch pane
(`b`). The checked-out branch cannot be deleted this way; git would refuse
regardless.

## Legend

| Glyph | Meaning |
|---|---|
| `✖N` | conflicted files |
| `⇅` | diverged from upstream |
| `↑N` | commits to push |
| `↓N` | commits to pull |
| `✚N` | staged changes |
| `●N` | unstaged changes |
| `?N` | untracked files |
| `⚑N` | stashes |
| `⌀` | detached HEAD |
| `⊘` | no upstream |
| `✔` | clean and in sync |

A branch that is not one of `main`, `master`, `trunk` or `develop` is shown in
bold rather than dimmed, so an unusual checkout is visible at a glance. The list
is configurable.

The selected row gets a `█` in the gutter and a dark band across the pane —
never an inverted row, which would turn every status colour into a background.
The band assumes a dark terminal; on a light one, change `Color::Selection` in
`src/theme.rs`.

Colour reinforces these but never replaces them: `NO_COLOR` and `--color=never`
are honoured, and colour is dropped automatically when stdout is not a
terminal.

## Development

```sh
make check    # fmt, clippy and the test suite
make install
```

## Configuration

See `config.example.toml`; copy it to `~/.config/gitdrift/config.toml`.
Everything is optional — with no config file, `gitdrift` scans `~/gitlab`.

## Pagers

`Shift+d`, `Shift+l`, `Shift+a`, `d` in the branch popover, and viewing a
stash all hand the terminal to a pager. gitdrift picks it
from `GIT_PAGER`, then `PAGER`, falling back to `less -R` — it does not use
`core.pager`, because a global `core.pager=` (a deliberate "never page" for
ordinary git use) would dump the output and return before the TUI had left
the screen.

## Mouse

The wheel scrolls the detail pane when the pointer is over it, and the
repository list otherwise. Mouse capture means your terminal's own text
selection needs <kbd>Shift</kbd> held while dragging.
