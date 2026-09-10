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

| Key | Action |
|---|---|
| `↑` `↓` `PgUp` `PgDn` | move |
| `g` / `G` | first / last |
| `n` | filter by namespace |
| `o` | cycle sort: drift, name, recent, stale |
| `Space` | mark / unmark |
| `V` | sweep the last mark's state to the cursor |
| `a` | mark all visible, or clear the marks |
| `f` / `F` | fetch marked-or-current / fetch all |
| `p` | `pull --ff-only`, marked-or-current |
| `P` | prune gone branches — asks first |
| `s` / `e` | shell / editor in the repo |
| `D` | diff `HEAD...@{u}` in your pager |
| `L` | log of the incoming commits, in your pager |
| `b` | switch branch on the current repo |
| `J` / `K` | scroll the detail pane — the title shows `▴▾` while there is more |
| `S` | browse stashes: view in your pager, drop — asks first |
| `x` | drop the selected stash — stash pane only, asks first |
| `r` | rescan |
| `d` | drifted only |
| `/` | filter |
| `!` | problems |
| `?` | help |
| `Esc` / `q` | quit |

`Space` marks rows; `f`, `p` and `P` then act on every marked
repository instead of the one under the cursor. `V` marks everything between the last
mark and the cursor, and `a` marks the whole visible list — or clears the
marks, if there are any. Marks survive a rescan, and only ever apply to
repositories still on screen: narrowing by namespace or filter narrows the
action too, and gitdrift says so rather than quietly doing nothing.

`n` opens a picker listing every top-level namespace (`idp`, `mkp`, `aws`, …)
with its repository count, plus "all". Pick one with `↑`/`↓` and `Enter`; it
beats scrolling a few hundred rows. `o` changes the order: drift (the
default), name, most recently committed, least recently fetched. The title
bar shows both.

`Esc` closes the filter or an open pane first, and quits only when there is
nothing left to close. The help and problems panes scroll with `↑`/`↓`,
`PgUp`/`PgDn` or the wheel when they outgrow the terminal.

## Destroying work

`P` throws work away, so it is uppercase and shows you what you are about to
lose before anything happens:

- `P` runs `git fetch --prune --prune-tags --force`, then `git branch -D` on
  every local branch whose upstream is gone. The checked-out branch and any
  branch checked out in another worktree are left alone.

This is not undoable. Press `y` to go through with it; any other key cancels.

Dropping a stash with `x` (from the stash pane) asks the same way, for the
same reason.

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

`D`, `L` and viewing a stash hand the terminal to a pager. gitdrift picks it
from `GIT_PAGER`, then `PAGER`, falling back to `less -R` — it does not use
`core.pager`, because a global `core.pager=` (a deliberate "never page" for
ordinary git use) would dump the output and return before the TUI had left
the screen.

## Mouse

The wheel scrolls the detail pane when the pointer is over it, and the
repository list otherwise. Mouse capture means your terminal's own text
selection needs <kbd>Shift</kbd> held while dragging.
