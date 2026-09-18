# gitdrift

A Rust TUI that finds the git repositories which have drifted — unpushed,
unpulled, dirty, stashed, conflicted — across a tree of several hundred of
them. Written for `~/gitlab`, which holds ~500 repos.

## Build and check

```sh
cargo test                                   # unit + integration
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo install --path . --force               # install the built binary
```

All three must be clean before a commit. `cargo test` is fast (< 2s); run it
rather than reasoning about whether a change was safe.

## Layout

| Path | What lives there |
|---|---|
| `src/discover.rs` | Walks the roots, finds repositories, honours the prune list |
| `src/status.rs` | One repository's status, read with `gix` |
| `src/scan.rs` | Parallel scan, streams `Event`s back to the caller |
| `src/detail.rs` | The deeper per-repository read behind the detail pane |
| `src/drift.rs` | `score()` and `is_drifted()` — the sort order |
| `src/theme.rs` | `Facet`, its glyph and its colour; the single source of both |
| `src/render.rs` | `--plain` and `--json` output, plus shared width helpers |
| `src/cli.rs` | Argument parsing and the non-TUI paths |
| `src/actions.rs` | Every mutation, shelled out to `git` |
| `src/ui/state.rs` | The state machine: `(state, event) → Command` |
| `src/ui/app.rs` | Terminal setup, the event loop, background threads |
| `src/ui/view.rs` | All rendering |
| `src/ui/picker.rs` | The cursor shared by the namespace, branch and stash popovers |

## Rules that hold the design together

**Reads use `gix`; every mutation shells out to the `git` binary.** Shelling
out preserves the user's SSH keys, credential helpers and `insteadOf` rules,
which no in-process implementation gets for free. The one deliberate
exception is `git diff --numstat` in `detail.rs`: a read, but not one `gix`
exposes conveniently.

**`src/ui/state.rs` must not import `crate::detail`.** It is a pure state
machine, testable without a terminal or a repository. Data the popovers need
is handed in by the event loop as `BranchChoice` / `StashChoice`.

**One key, one binding.** In `App::on_key` the Ctrl-C check and the modifier
guard (rejecting Ctrl/Alt/Super/Meta) are the first two statements, so a
chord can never fall through to a plain-key branch.

**Meaning survives `NO_COLOR`.** Every colour is paired with a unique glyph —
there is a test enforcing glyph uniqueness — and the selection cursor is a
gutter bar plus bold, never an inverted row.

**Destructive actions go through the confirm pane** and only lowercase `y`
proceeds. Today that is `Shift+x` (prune) and `x` (drop a stash).

**Popovers carry their own repository path.** `App::picker_repo` exists
because `LoopState::current_detail` is not cleared on cursor move: between a
move and the next `Msg::Detail`, a popover would otherwise list one repo and
act on another.

**Pagers: gitdrift supplies its own** (`GIT_PAGER` → `PAGER` → `less -R`) via
`-c core.pager=…`. It cannot rely on `core.pager`, because the user's global
config sets it empty, which means "never page" and would return before the
TUI had left the screen.

## Testing

`ratatui::backend::TestBackend` renders frames headlessly; `tests/support`
builds real repositories in a tempdir for the integration tests. When fixing
a bug, verify the regression test actually fails against the old behaviour.

**Never use `git checkout <file>` to do that** — copy the file to the
scratchpad and restore from there. `git checkout` has destroyed uncommitted
work in this repo before.

**Never run mutating git commands against `~/gitlab`, `~/github`, or any
repository outside a fixture tempdir**, and never point gitdrift's fetch,
pull or prune at the user's real trees. `--plain` and `--json` are read-only
and safe.

No agent has a tty, so anything involving the alternate screen, the pager
handoff or the mouse can only be verified by the user.

## Conventions

Design specs and plans live in `docs/`. Commit messages and comments follow
the user's global CLAUDE.md: lead with what changed and why, comment only
what is genuinely non-obvious, and never narrate a change in the code.
