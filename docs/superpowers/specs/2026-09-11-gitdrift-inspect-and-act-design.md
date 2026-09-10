# gitdrift — inspecting and acting on drift

Date: 2026-09-11
Status: approved, pending implementation plan
Supersedes: parts of `2026-09-10-gitdrift-design.md` (see *Changes to the
original design* below)

## Problem

gitdrift now tells you which repositories have drifted. It does not tell you
*what* the drift is, and it cannot act on the two cases that matter most.

Concretely, after a day away:

- `↓3` says three commits are incoming. It does not say which files change.
  The information a `git pull` prints — the file list with `+/-` stats, and the
  `abc1234..def5678` range you can diff — is exactly what is wanted, and is
  currently only obtainable by leaving the tool.
- Switching branches and clearing stashes means dropping to a shell, one
  repository at a time.
- `F` (fetch all) leaves every row showing pre-fetch ahead/behind until `r` is
  pressed by hand. There is no indication that work is in flight, so a stale
  screen and a busy screen look identical.
- Marks can be set in bulk (`V`) but only cleared one at a time or all at once.
- Two destructive actions — discard tracked changes (`X`) and delete untracked
  files (`C`) — duplicate what a shell does better, and carry real risk for
  little gain.

## Goals

- See what is incoming without leaving the tool, and read the full diff in the
  user's own pager with the user's own git configuration.
- Switch branch and manage stashes on the selected repository.
- Always know whether the screen is current or work is in progress.
- Symmetric bulk marking: anything that can be marked in a sweep can be
  unmarked in a sweep.
- Less surface: remove the two destructive actions that earn their risk least.

## Non-goals

Rendering diffs inside gitdrift. Push, merge, rebase, cherry-pick. Creating,
applying or popping stashes. Creating branches, or checking out remote branches
that have no local counterpart. Acting on more than one repository for any of
the new inspection actions.

## Changes to the original design

- Non-goal "Creating, applying or dropping stashes" is narrowed: **dropping** a
  stash is now in scope, behind a confirmation. Creating, applying and popping
  remain out.
- `X` (discard) and `C` (clean) are removed from the keymap, the command set
  and `actions.rs`.

---

## 1. Removing discard and clean

Delete, with no replacement:

| Thing | Location |
|---|---|
| `Command::Discard`, `Command::Clean` | `ui/state.rs` |
| `Destructive::Discard`, `Destructive::Clean` | `ui/state.rs` |
| `X` and `C` bindings | `ui/state.rs` |
| `actions::discard_changes`, `actions::clean_untracked` | `actions.rs` |
| loop arms for both commands | `ui/app.rs` |
| footer hints, help pane rows, README rows | `ui/view.rs`, `README.md` |

`Destructive` becomes `Prune | StashDrop`. The confirm pane stays as-is.

`confirm()` currently counts affected files per kind. With discard and clean
gone, neither remaining kind has a meaningful up-front file count (prune's
branch list is only known after its fetch; a stash is one entry). The count
drops out of the summary entirely:

```
prune remotes and delete branches whose upstream is gone in 12 repositories
drop stash@{2} in idp/run-plane/api — "wip: retry backoff"
```

## 2. Activity indicator

### State

```rust
pub struct Job {
    pub id: u64,
    pub label: String,   // "fetching", "scanning", "pruning"
    pub done: usize,
    pub total: usize,    // 0 = indeterminate (a scan does not know its size)
}
```

`App` holds `jobs: Vec<Job>` and `next_job_id: u64`, with:

- `begin_job(label, total) -> u64` — called on the UI thread when work is
  spawned, returns the id the worker reports against.
- `advance_job(id)` — `done += 1`.
- `end_job(id)` — removes it.

The existing `scanning: bool` is replaced by a job id: `App` holds
`scan_job: Option<u64>`, set in `begin_scan()` and cleared in `finish_scan()`,
and `is_scanning()` returns `self.scan_job.is_some()`. Job labels are display
text only — nothing branches on them.

### Message plumbing

Two new variants in `Msg`:

```rust
JobStep(u64),   // one unit of a job finished
JobEnd(u64),
```

Workers are handed their job id at spawn time. `spawn_fetch` sends a `JobStep`
per repository, `JobEnd` at the end.

### Rendering

The footer's left edge, ahead of the hints:

```
⠹ fetching 37/511   ↑↓ move  n namespace  …        ? help  Esc quit
⠴ scanning 214      ↑↓ move  …                     ? help  Esc quit
⠦ fetching 37/511 +1                               ? help  Esc quit
```

- Frame from elapsed time (`Instant`), not stored state — the loop already
  redraws every 80 ms. Frames: `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`.
- `total == 0` renders just `done`.
- More than one job renders the first plus `+N`.
- The indicator competes with the hints for width; `footer_hints` already drops
  hints from the tail to fit, and the indicator is prepended to its budget.
- **No colour dependence:** the glyph animates, so motion carries the meaning
  under `NO_COLOR`.

## 3. Fetch refreshes rows; `F` rescans

### Per-row refresh

`spawn_fetch` takes `Vec<(PathBuf, String)>` (path plus display name, via the
existing `named()` helper) instead of `Vec<PathBuf>`. After each successful
fetch the worker sends `Msg::Refresh(status::inspect(path, name), String::new())`
— the same message `spawn_pull` already uses. Rows therefore update *during* a
fetch-all, not in one jump at the end.

### Full rescan after fetch-all

`LoopState` gains `rescan_after: Option<u64>` — the job id whose completion
owes a rescan, set when `Command::FetchAll` spawns its fetch. When `Msg::JobEnd`
arrives for that id, the loop performs the same work as `Command::Rescan` (bump
generation, `begin_scan`, `spawn_scan`). This catches repositories cloned or removed since startup, which
per-row refresh cannot see.

Plain `f` does not rescan — re-inspecting its own rows is enough, and a full
rescan of 511 repositories to learn about three is waste.

### Rescan preserves the selection

Today `begin_scan()` clears `rows` and sets `selected = 0`, so any rescan —
manual `r` included — throws the cursor to the top of the list. This becomes
disruptive once a rescan can be triggered for the user.

`begin_scan()` remembers the selected repository's **path**; `finish_scan()`
restores the selection to that path's new index, or leaves it at 0 if the
repository is gone. Marks already survive a rescan by path and are unchanged.

## 4. Incoming changes and the diff

### Collecting

`detail.rs` gains one subprocess call, made only when the selected repository
has `behind > 0`:

```
git diff --numstat HEAD...@{u}
```

Three dots deliberately: against a diverged branch, `HEAD..@{u}` describes the
wrong thing (it inverts the local commits), while `HEAD...@{u}` is what a pull
would actually bring in. For a fast-forward the two are identical.

```rust
pub struct FileDelta { pub added: u32, pub removed: u32, pub path: String }

pub struct Incoming {
    pub files: Vec<FileDelta>,   // capped at CHANGED_LIMIT
    pub truncated: bool,
    pub total_added: u32,
    pub total_removed: u32,
    pub range: String,           // "abc1234..def5678"
}
```

`RepoDetail` gains `incoming: Option<Incoming>`.

Parsing `--numstat` is a pure function — `parse_numstat(&str) -> Vec<FileDelta>`
— tested on its own. Binary files report `-` for both counts and are recorded
as `0/0`; a rename line (`a => b`) keeps the raw path text.

This is the first subprocess in `detail.rs`, which is otherwise pure `gix`
reads. Justified: `gix` has no diff-stat equivalent worth building here, the
call is per-selection rather than per-repository, and it already runs off the UI
thread in `spawn_detail`. A failure leaves `incoming: None`, consistent with
every other section of `inspect()`.

### Rendering

In the detail pane, below the branch section, when `incoming` is present:

```
incoming  abc1234..def5678
  src/api/users.rs      | 42 ++++---
  src/db/schema.sql     |  8 ++
  tests/users_test.rs   | 61 +++++++
  3 files, 94 insertions(+), 17 deletions(-)
  D diff   L log
```

Bars are scaled to the column width, `+` in the added facet colour, `-` in the
removed one — with the `+`/`-` characters themselves carrying the meaning under
`NO_COLOR`.

### The pager

Two new commands, both single-repository, both routed through the existing
`suspend()`:

| Key | Command | Runs |
|---|---|---|
| `D` | `Command::Diff(PathBuf)` | `git diff HEAD...@{u}` |
| `L` | `Command::Log(PathBuf)` | `git log --stat HEAD..@{u}` |

`suspend()` restores the real terminal, so git sees a tty on stdout and invokes
the user's pager — `core.pager`, delta, `less` options, colour settings all
apply untouched. On return the TUI is re-entered and cleared, exactly as `s` and
`e` already do.

Both are refused with a toast when the repository has no upstream. `L` uses two
dots: the commit list that is incoming, not a symmetric difference.

Implemented in `actions.rs` as `run_git_interactive(path, args)` — spawns `git`
with inherited stdio and waits — distinct from the existing `run_git`, which
captures output.

## 5. Branch switcher — `b`

### Data

`detail.rs` already collects `Vec<BranchInfo>` (name, upstream, ahead, behind,
is_head) for the selected repository. Nothing new is gathered.

The pure state machine must not depend on `detail.rs`, so the branch list is
handed to it rather than read by it:

1. `b` returns `Command::OpenBranches`.
2. The event loop, which holds `current_detail`, calls
   `app.open_branch_picker(rows)` with a display-ready `Vec<BranchChoice>`.
3. If no detail has loaded yet, the loop toasts `"still loading…"` and the pane
   does not open.

### Behaviour

```
┌─ switch branch — idp/run-plane/api ───┐
│ > main            ↑0 ↓3   (current)   │
│   feat/retries    ↑2                  │
│   fix/timeout     ↑1 ↓4               │
│   spike/cache     (no upstream)       │
│ ↑↓ move   Enter switch   Esc close    │
└───────────────────────────────────────┘
```

- Refused with a toast if the worktree is dirty (`staged + unstaged +
  conflicted > 0`), reusing `ActionError::Dirty`. Untracked files do not block
  a switch — same rule as `can_pull`.
- Choosing the current branch closes the pane and does nothing.
- `Enter` → `Command::Checkout(path, branch)` → `git switch <branch>` off the
  UI thread → `Msg::Refresh` with the re-inspected row.
- Single repository only: it acts on the selected row, never on the marked set.
  A branch name is not meaningful across repositories.

## 6. Stash browser — `S`

Same picker mechanism, same hand-in pattern (`Command::OpenStashes`, loop
supplies `Vec<StashEntry>` from `current_detail`).

```
┌─ stashes — idp/run-plane/api ─────────────────┐
│ > stash@{0}  wip: retry backoff               │
│   stash@{1}  On main: debugging the timeout   │
│ ↑↓ move  Enter view  x drop  Esc close        │
└───────────────────────────────────────────────┘
```

| Key | Effect |
|---|---|
| `Enter` | `git stash show -p stash@{i}` in the pager, via `suspend()` |
| `x` | confirmation, then `git stash drop stash@{i}`, then refresh |
| `Esc` | close |

`x` routes through the existing confirm pane as `Destructive::StashDrop`.
`Pending` gains `stash: Option<usize>` — `None` for a prune, the stash index
for a drop — rather than overloading `targets`, which stays a list of
repository paths (of length one here). Dropping re-requests detail for the row,
because every later stash's index shifts down by one.

Empty stash list: `S` toasts `"no stashes"` and does not open the pane.

No apply, no pop, no create — `s` opens a shell in the repository, where those
belong.

## 7. Symmetric range marks

`Space` already toggles a row and sets the anchor. It additionally records what
the toggle *did*:

```rust
anchor: Option<(usize, bool)>,   // index, and whether that row ended up marked
```

`V` applies the anchor's resulting state to every row between anchor and cursor
inclusive — marking the range if `Space` marked its anchor, unmarking it if
`Space` unmarked it.

With no anchor (nothing toggled since the last filter/sort/rescan change), `V`
marks the current row only, as it does today.

`a` (mark all / clear all) is unchanged. The anchor continues to be cleared
whenever the visible set changes — filter, namespace, sort, drifted-only,
rescan — because a stale index refers to a different row.

## 8. Extracting the picker

Three panes are now the same object: a bounded cursor over a list of labelled
rows, opened with a preselected index, moved with `↑`/`↓`, chosen with `Enter`,
closed with `Esc`.

`src/ui/picker.rs`:

```rust
pub struct Picker { pub index: usize, pub len: usize }

impl Picker {
    pub fn open(len: usize, at: usize) -> Picker;
    pub fn move_by(&mut self, delta: isize);
    pub fn chosen(&self) -> usize;
}
```

`App` holds one `picker: Option<Picker>` shared by all three panes; which pane
is open already distinguishes them. The namespace picker is migrated to it as
part of this work, so there is one implementation rather than three.

Rendering is likewise one function — `draw_picker(frame, area, title, rows,
index, footer)` — with the three call sites differing only in title, row
formatting and footer text. `draw_namespaces` becomes a caller.

## Keymap after this change

Removed: `X`, `C`.
Added: `D` diff, `L` log, `b` branch, `S` stash, `x` (drop, inside the stash
pane only).

Every binding remains a single key with a single meaning. No lowercase alias
for an uppercase key, no key doing two jobs in one pane.

## Testing

Everything below the terminal is testable; the TUI itself is rendered headless
with `ratatui::backend::TestBackend`.

**Pure functions**
- `parse_numstat`: normal lines, binary (`-`/`-`), renames, empty input.
- `Picker::move_by` clamps at both ends; `open` clamps an out-of-range index.

**State machine** (`ui/state.rs`)
- `V` after a marking `Space` marks the range; `V` after an unmarking `Space`
  unmarks it.
- `V` with no anchor marks only the current row.
- Anchor is cleared by a filter, sort, namespace or drifted-only change.
- `X` and `C` produce no command.
- `b` with a dirty worktree toasts and opens nothing.
- `S` with no stashes toasts and opens nothing.
- Choosing the current branch closes the pane and emits no command.
- `x` in the stash pane stages a confirmation rather than dropping.
- Job accounting: begin/advance/end, `is_scanning` derived from jobs.

**Rendering** (`ui/view.rs`)
- Incoming block renders file names, counts and the range.
- Spinner renders with its counter, and yields to hints when the pane is narrow.
- Each picker renders its title, rows and footer.

**Regression discipline**
Every test written for a bug or a behaviour change is verified to fail when its
production change is reverted — reverted via a scratchpad copy of the file,
never `git checkout`.

**Not covered by tests** (requires a tty; the user verifies by hand)
`D`, `L` and stash-view handing off to the pager and returning cleanly; the
spinner animating; a real `git switch` and `git stash drop`.

## Risks

- **`git switch` and `git stash drop` mutate a real repository.** Both are
  guarded — switch by the dirty check, drop by a confirmation — and both are
  single-repository. Drop is the only irreversible one; `git stash drop` prints
  the dropped commit id, which the toast will carry so the user can recover it
  via `git stash store`.
- **A subprocess in `detail.rs`** adds latency to the detail pane on a repo
  that is behind. It runs off the UI thread and only when `behind > 0`; a slow
  or failed call leaves the section absent rather than blocking the pane.
- **Pager handoff leaves the terminal in a bad state if git dies oddly.** The
  existing `suspend()` path and the panic hook already cover this for `s` and
  `e`; the new callers reuse both.
