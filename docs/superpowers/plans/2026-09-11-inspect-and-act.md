# gitdrift inspect-and-act Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let gitdrift show what a pull would bring, switch branches, manage stashes, and say out loud when it is busy — while dropping the two destructive actions a shell does better.

**Architecture:** The pure `(state, event) → Command` state machine in `src/ui/state.rs` stays pure: it never reads `detail.rs` or spawns work. New popover panes receive their rows from the event loop, which owns the loaded detail. Diffs are never rendered by gitdrift — `suspend()` hands the terminal back to git so the user's own pager and git config apply. Three near-identical popovers collapse onto one `Picker` cursor type.

**Tech Stack:** Rust 2021 (rust-version 1.85), ratatui 0.30, crossterm 0.29, gix 0.87 (reads only), `git` binary for every mutation, `ratatui::backend::TestBackend` for headless render tests.

**Spec:** `docs/superpowers/specs/2026-09-11-gitdrift-inspect-and-act-design.md`

## Global Constraints

- **Reads use `gix`; every mutation shells out to the `git` binary.** This is what keeps the user's SSH keys, credential helpers and `insteadOf` rules working. The one deliberate exception added here is `git diff --numstat` (a read) — justified in the spec, §4.
- **One key, one binding.** No lowercase alias for an uppercase key. No key doing two jobs within one pane. The modifier guard at the top of `App::on_key` already rejects Ctrl/Alt/Super/Meta chords; do not weaken it.
- **Meaning must survive `NO_COLOR`.** Every colour carries a unique glyph or character alongside it (`+`/`-`, `↑`/`↓`, `●`). The spinner carries meaning through motion.
- **`src/ui/state.rs` must not import `crate::detail`.** Panes that need detail data receive it via an `open_*_picker` call from the event loop.
- **Destructive actions go through the confirm pane**, and only lowercase `y` proceeds — the pane's own text promises "any other key cancels".
- **Every regression test is verified to fail when its production change is reverted.** Revert by copying the file to the scratchpad and back — **never** `git checkout <file>`, which has silently destroyed uncommitted work in this repo before.
- **Comment style:** comment only subtle or non-obvious code, one or two lines. Never narrate the change or restate what the code says.
- **Commit messages end with:** `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`
- **Gate for every task:** `cargo test`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` must all pass before the commit.

---

### Task 1: Remove discard and clean

Strict subtraction. Doing it first shrinks `Destructive` before Task 10 grows it again.

**Files:**
- Modify: `src/actions.rs` (delete `discard_changes`, `clean_untracked`)
- Modify: `src/ui/state.rs` (commands, `Destructive`, `confirm`, keymap, help)
- Modify: `src/ui/app.rs` (loop arms)
- Modify: `src/ui/view.rs` (`HINTS`, `draw_help`)
- Modify: `README.md`
- Test: `tests/actions.rs` (drop the tests for the deleted functions), `src/ui/state.rs` tests

**Interfaces:**
- Consumes: nothing.
- Produces: `Destructive` is now `enum Destructive { Prune }`. `Pending` keeps `{ kind, targets, summary }` for now; Task 10 adds `stash`.

- [ ] **Step 1: Write the failing test**

In `src/ui/state.rs`, in `mod tests`:

```rust
#[test]
fn the_removed_destructive_keys_do_nothing() {
    let mut app = app_with(vec![row("a", 1)]);
    for c in ['X', 'C'] {
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)),
            Command::None,
            "{c} should no longer be bound"
        );
        assert_eq!(app.pane(), Pane::List, "{c} should not open a confirmation");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib the_removed_destructive_keys`
Expected: FAIL — `X` opens the confirm pane, so `app.pane()` is `Pane::Confirm`.

- [ ] **Step 3: Delete the feature**

In `src/actions.rs`, delete `discard_changes` and `clean_untracked` entirely (with their doc comments).

In `src/ui/state.rs`:
- Delete the `Discard(Vec<PathBuf>)` and `Clean(Vec<PathBuf>)` variants of `Command`.
- Reduce `Destructive` to one variant and simplify `verb`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destructive {
    Prune,
}

impl Destructive {
    pub fn verb(self) -> &'static str {
        match self {
            Destructive::Prune => "prune remotes and delete branches whose upstream is gone in",
        }
    }
}
```

- Delete the `KeyCode::Char('X')` and `KeyCode::Char('C')` arms from the list keymap, and fix the comment above `'P'` so it reads:

```rust
            // Uppercase only: this destroys work, so a mistyped lowercase key
            // must not reach it. It then waits on a confirmation.
            KeyCode::Char('P') => self.confirm(Destructive::Prune),
```

- In `confirm`, the per-kind file count no longer has a meaningful case. Delete the `affected` computation and the `match kind` on the summary:

```rust
    fn confirm(&mut self, kind: Destructive) -> Command {
        let targets = self.targets();
        if targets.is_empty() {
            self.warn_no_targets();
            return Command::None;
        }
        let scope = if targets.len() == 1 {
            self.visible()
                .iter()
                .find(|r| r.path == targets[0])
                .map(|r| r.display_name.clone())
                .unwrap_or_else(|| targets[0].display().to_string())
        } else {
            format!("{} repositories", targets.len())
        };
        self.pending = Some(Pending {
            kind,
            targets,
            summary: format!("{} {scope}", kind.verb()),
        });
        self.pane = Pane::Confirm;
        Command::None
    }
```

- In `on_key_overlay`'s confirm branch, the match collapses:

```rust
            return match (confirmed, pending) {
                (true, Some(p)) => match p.kind {
                    Destructive::Prune => Command::Prune(p.targets),
                },
                _ => Command::None,
            };
```

In `src/ui/app.rs`, delete the `Command::Discard(paths)` and `Command::Clean(paths)` arms.

In `src/ui/view.rs`, delete these entries from `HINTS`:

```rust
    ("X", "discard"), ("C", "clean"),
```

and the matching rows from `draw_help`'s `bindings` array (the `X` and `C` lines).

In `tests/actions.rs`, delete `clean_untracked` and `discard_changes` from the `use` list and delete every test that calls them.

In `README.md`, delete the `X` and `C` rows from the keybinding table and any prose describing them.

- [ ] **Step 4: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings. `grep -rn "discard_changes\|clean_untracked" src tests README.md` returns nothing.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: drop discard and clean

Both duplicate what a shell does better and carry real risk for little
gain. Prune keeps the confirmation pane.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 2: Extract the picker cursor

Three popovers are the same object. Build it once, migrate namespaces onto it now, and Tasks 9 and 10 are cheap.

**Files:**
- Create: `src/ui/picker.rs`
- Modify: `src/ui/mod.rs`, `src/ui/state.rs`, `src/ui/view.rs`
- Test: `src/ui/picker.rs` (`mod tests`), `src/ui/state.rs` tests

**Interfaces:**
- Consumes: Task 1's `Destructive`.
- Produces:
  - `gitdrift::ui::picker::Picker` with `open(len: usize, at: usize) -> Picker`, `move_by(&mut self, delta: isize)`, `index(&self) -> usize`, `len(&self) -> usize`, `is_empty(&self) -> bool`.
  - `App::picker_index(&self) -> usize` replaces `App::namespace_index`.
  - `view::draw_picker(frame, area, title: &str, rows: Vec<Line<'static>>, index: usize, footer: &str)`.

- [ ] **Step 1: Write the failing test**

Create `src/ui/picker.rs` containing only its test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_clamps_at_both_ends() {
        let mut p = Picker::open(3, 0);
        p.move_by(-1);
        assert_eq!(p.index(), 0);
        p.move_by(10);
        assert_eq!(p.index(), 2);
    }

    #[test]
    fn opening_past_the_end_lands_on_the_last_row() {
        assert_eq!(Picker::open(3, 99).index(), 2);
    }

    #[test]
    fn an_empty_picker_has_no_cursor_to_move() {
        let mut p = Picker::open(0, 0);
        p.move_by(1);
        assert_eq!(p.index(), 0);
        assert!(p.is_empty());
    }
}
```

Add `pub mod picker;` to `src/ui/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib picker`
Expected: FAIL to compile — `Picker` is not defined.

- [ ] **Step 3: Write the implementation**

At the top of `src/ui/picker.rs`:

```rust
/// A bounded cursor over a list of rows. Which pane is open is what gives
/// the rows their meaning, so the cursor itself carries none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    index: usize,
    len: usize,
}

impl Picker {
    pub fn open(len: usize, at: usize) -> Picker {
        Picker {
            index: at.min(len.saturating_sub(1)),
            len,
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.len == 0 {
            return;
        }
        let max = (self.len - 1) as isize;
        self.index = (self.index as isize + delta).clamp(0, max) as usize;
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib picker`
Expected: PASS (3 tests).

- [ ] **Step 5: Migrate the namespace pane onto it**

In `src/ui/state.rs`:
- `use crate::ui::picker::Picker;`
- Replace the field `ns_index: usize` with `picker: Option<Picker>`; update `App::new` (`picker: None`).
- Replace `namespace_index`:

```rust
    /// Cursor position in whichever popover is open.
    pub fn picker_index(&self) -> usize {
        self.picker.as_ref().map_or(0, Picker::index)
    }
```

- `open_namespaces` becomes:

```rust
    fn open_namespaces(&mut self) {
        let choices = self.namespace_choices();
        let at = match &self.group {
            None => 0,
            Some(g) => choices.iter().position(|(n, _)| n == g).unwrap_or(0),
        };
        self.picker = Some(Picker::open(choices.len(), at));
        self.pane = Pane::Namespaces;
    }
```

- `choose_namespace` reads `self.picker_index()` instead of `self.ns_index`, and sets `self.picker = None` alongside `self.pane = Pane::List`.
- In `on_key_overlay`, the `Pane::Namespaces` branch moves the cursor through the picker:

```rust
        if self.pane == Pane::Namespaces {
            match key.code {
                KeyCode::Down => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_by(1);
                    }
                    return Command::None;
                }
                KeyCode::Up => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_by(-1);
                    }
                    return Command::None;
                }
                KeyCode::Enter => return self.choose_namespace(),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => {
                    self.pane = Pane::List;
                    self.picker = None;
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
```

In `src/ui/view.rs`, add the shared renderer and make `draw_namespaces` call it:

```rust
/// The one popover renderer. Callers differ only in title, row text and footer.
fn draw_picker(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: Vec<Line<'static>>,
    index: usize,
    footer: &str,
) {
    let body_w = rows
        .iter()
        .map(|l| l.width())
        .chain([title.chars().count(), footer.chars().count()])
        .max()
        .unwrap_or(10);
    let width = (body_w + 4) as u16;
    let height = (rows.len() + 3) as u16;
    let popup = centered(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(format!(" {title} "));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let list = Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let mut state = ListState::default();
    state.select(Some(index));
    frame.render_stateful_widget(
        List::new(rows.into_iter().map(ListItem::new).collect::<Vec<_>>())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        list,
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer.to_string(), dim()))),
        Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: 1,
            ..inner
        },
    );
}

fn draw_namespaces(frame: &mut Frame, app: &App, area: Rect) {
    let choices = app.namespace_choices();
    let name_w = choices
        .iter()
        .map(|(n, _)| namespace_label(n).chars().count())
        .max()
        .unwrap_or(3);
    let count_w = choices
        .iter()
        .map(|(_, c)| c.to_string().len())
        .max()
        .unwrap_or(1);

    let rows: Vec<Line<'static>> = choices
        .iter()
        .map(|(name, count)| {
            let active = app.group().unwrap_or("all") == name;
            let label = namespace_label(name);
            let mut style = Style::default().add_modifier(Modifier::BOLD);
            if active {
                style = style.fg(theme::Color::Accent.to_ratatui());
            }
            Line::from(vec![
                Span::styled(if active { "● " } else { "  " }, accent()),
                Span::styled(format!("{label:name_w$}"), style),
                Span::styled(format!("  {count:>count_w$}"), dim()),
            ])
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Namespace",
        rows,
        app.picker_index(),
        "↑↓ move   Enter choose   Esc close",
    );
}
```

Fix the one existing view test that asserts on the namespace popover if its expected text moved.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS. Every pre-existing namespace-picker test in `src/ui/state.rs` still passes unchanged — that is the point of the migration.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
refactor: one picker cursor, one popover renderer

Namespaces move onto it now; the branch and stash pickers land on it next
rather than copying it twice.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 3: Job accounting and the spinner

**Files:**
- Modify: `src/ui/state.rs` (`Job`, job methods, `scan_job`)
- Modify: `src/ui/view.rs` (`activity`, `spinner_frame`, `draw_footer`)
- Test: `src/ui/state.rs` tests, `src/ui/view.rs` tests

**Interfaces:**
- Consumes: Task 2.
- Produces:
  - `pub struct Job { pub id: u64, pub label: String, pub done: usize, pub total: usize }`
  - `App::begin_job(&mut self, label: impl Into<String>, total: usize) -> u64`
  - `App::advance_job(&mut self, id: u64)`, `App::end_job(&mut self, id: u64)`, `App::jobs(&self) -> &[Job]`
  - `App::is_scanning` is unchanged in signature and now derives from `scan_job`.

- [ ] **Step 1: Write the failing tests**

In `src/ui/state.rs` tests:

```rust
#[test]
fn a_job_counts_up_and_disappears_when_it_ends() {
    let mut app = App::new();
    app.finish_scan();
    let id = app.begin_job("fetching", 3);
    app.advance_job(id);
    app.advance_job(id);
    assert_eq!(app.jobs()[0].done, 2);
    assert_eq!(app.jobs()[0].total, 3);
    app.end_job(id);
    assert!(app.jobs().is_empty());
}

#[test]
fn advancing_an_ended_job_is_not_a_panic() {
    let mut app = App::new();
    app.finish_scan(); // App::new starts a scan job; clear it first
    let id = app.begin_job("fetching", 1);
    app.end_job(id);
    app.advance_job(id);
    assert!(app.jobs().is_empty());
}

#[test]
fn scanning_is_a_job_that_counts_the_rows_it_finds() {
    let mut app = App::new();
    app.begin_scan();
    assert!(app.is_scanning());
    app.push_status(row("a", 1));
    let scan = app.jobs().iter().find(|j| j.label == "scanning").unwrap();
    assert_eq!((scan.done, scan.total), (1, 0), "a scan does not know its size");
    app.finish_scan();
    assert!(!app.is_scanning());
    assert!(app.jobs().is_empty());
}
```

In `src/ui/view.rs` tests:

```rust
#[test]
fn the_activity_line_shows_the_label_and_its_progress() {
    let jobs = [Job { id: 1, label: "fetching".into(), done: 37, total: 511 }];
    assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ fetching 37/511");
}

#[test]
fn an_indeterminate_job_shows_only_what_it_has_done() {
    let jobs = [Job { id: 1, label: "scanning".into(), done: 214, total: 0 }];
    assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ scanning 214");
}

#[test]
fn extra_jobs_are_counted_not_listed() {
    let jobs = [
        Job { id: 1, label: "fetching".into(), done: 1, total: 2 },
        Job { id: 2, label: "pruning".into(), done: 0, total: 4 },
    ];
    assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ fetching 1/2 +1");
}

#[test]
fn there_is_no_activity_line_when_nothing_is_running() {
    assert!(activity(&[], '⠹').is_none());
}

#[test]
fn the_spinner_advances_over_time() {
    assert_ne!(
        spinner_frame(Duration::from_millis(0)),
        spinner_frame(Duration::from_millis(100))
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib job && cargo test --lib activity`
Expected: FAIL to compile — `Job`, `begin_job`, `activity`, `spinner_frame` are not defined.

- [ ] **Step 3: Implement job accounting**

In `src/ui/state.rs`, above `App`:

```rust
/// Background work in flight. `total == 0` means the size is not yet known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub label: String,
    pub done: usize,
    pub total: usize,
}
```

Replace the `scanning: bool` field with:

```rust
    jobs: Vec<Job>,
    next_job_id: u64,
    /// The scan's job, if one is running. Labels are display text only —
    /// nothing branches on them.
    scan_job: Option<u64>,
```

Delete the `scanning: true` initialiser. A fresh `App` must still report that it is scanning, so `App::new` binds the struct and starts the job before returning it — change its tail from `App { … }` to:

```rust
        let mut app = App {
            // …every existing field initialiser, unchanged, minus `scanning`…
            jobs: Vec::new(),
            next_job_id: 0,
            scan_job: None,
            default_branches: crate::theme::DEFAULT_BRANCHES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        };
        app.scan_job = Some(app.begin_job("scanning", 0));
        app
```

Then the methods:

```rust
    pub fn begin_job(&mut self, label: impl Into<String>, total: usize) -> u64 {
        self.next_job_id += 1;
        let id = self.next_job_id;
        self.jobs.push(Job {
            id,
            label: label.into(),
            done: 0,
            total,
        });
        id
    }

    pub fn advance_job(&mut self, id: u64) {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
            j.done += 1;
        }
    }

    pub fn end_job(&mut self, id: u64) {
        self.jobs.retain(|j| j.id != id);
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn is_scanning(&self) -> bool {
        self.scan_job.is_some()
    }
```

In `begin_scan`, replace `self.scanning = true;` with:

```rust
        if let Some(id) = self.scan_job.take() {
            self.end_job(id);
        }
        self.scan_job = Some(self.begin_job("scanning", 0));
```

In `finish_scan`, replace `self.scanning = false;` with:

```rust
        if let Some(id) = self.scan_job.take() {
            self.end_job(id);
        }
```

In `push_status`, after the row is inserted, count it:

```rust
        if let Some(id) = self.scan_job {
            self.advance_job(id);
        }
```

- [ ] **Step 4: Implement the spinner**

In `src/ui/view.rs`, add `use crate::ui::state::Job;` and `use std::sync::OnceLock;` / `use std::time::{Duration, Instant};` as needed, then:

```rust
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn spinner_frame(elapsed: Duration) -> char {
    SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()]
}

/// Time since the first frame. The loop already redraws every 80 ms, so the
/// animation needs no counter threaded through the state.
fn since_start() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// One line naming the work in flight, or `None` when the screen is current.
fn activity(jobs: &[Job], frame: char) -> Option<String> {
    let first = jobs.first()?;
    let count = if first.total == 0 {
        first.done.to_string()
    } else {
        format!("{}/{}", first.done, first.total)
    };
    let extra = match jobs.len() {
        1 => String::new(),
        n => format!(" +{}", n - 1),
    };
    Some(format!("{frame} {} {count}{extra}", first.label))
}
```

Rewrite `draw_footer` so a running job outranks a toast — a result toast arrives only once its job has ended, so nothing is lost:

```rust
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let spans = if let Some(filter) = app.filter_text() {
        vec![
            Span::styled("/", accent()),
            Span::raw(filter.to_string()),
            Span::styled("▏", accent()),
        ]
    } else if let Some(text) = activity(app.jobs(), spinner_frame(since_start())) {
        let used = text.chars().count() + 2;
        vec![
            Span::styled(format!("{text}  "), accent()),
            Span::styled(footer_hints(width.saturating_sub(used)), dim()),
        ]
    } else if let Some(toast) = app.toast() {
        vec![Span::styled(toast.to_string(), accent())]
    } else {
        vec![Span::styled(footer_hints(width), dim())]
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS. Existing tests that call `App::new()` then `finish_scan()` continue to work because `new` starts a scan job that `finish_scan` ends.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: show what is running

A spinner and a live counter in the footer, fed by a list of in-flight
jobs. Scanning is now one of those jobs rather than its own flag, so a
busy screen never looks like a stale one.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 4: Wire fetch to jobs, per-row refresh and the rescan

**Files:**
- Modify: `src/ui/app.rs` (`Msg`, `LoopState`, loop arms, `spawn_fetch`, `spawn_worktree_op`)
- Modify: `src/ui/state.rs` (`begin_scan`/`finish_scan` selection restore)
- Test: `src/ui/state.rs` tests

**Interfaces:**
- Consumes: Task 3's job API.
- Produces:
  - `Msg::JobStep(u64)`, `Msg::JobEnd(u64)`.
  - `spawn_fetch(targets: Vec<(PathBuf, String)>, concurrency: usize, job: u64, tx: mpsc::Sender<Msg>)`
  - `spawn_worktree_op(targets, op, past_tense, job: u64, tx)`
  - `LoopState.rescan_after: Option<u64>`
  - `fn do_rescan(app: &mut App, ls: &mut LoopState, opts: &ScanOptions, tx: &mpsc::Sender<Msg>)`

- [ ] **Step 1: Write the failing test**

In `src/ui/state.rs` tests:

```rust
#[test]
fn a_rescan_keeps_the_cursor_on_the_same_repository() {
    let mut app = app_with(vec![row("alpha", 1), row("beta", 1), row("gamma", 1)]);
    app.on_key(code(KeyCode::Down));
    app.on_key(code(KeyCode::Down));
    let before = app.selected().unwrap().path.clone();

    app.begin_scan();
    // The scan comes back in a different order; the cursor follows the repo.
    app.push_status(row("gamma", 1));
    app.push_status(row("alpha", 1));
    app.push_status(row("beta", 1));
    app.finish_scan();

    assert_eq!(app.selected().unwrap().path, before);
}

#[test]
fn a_rescan_that_loses_the_selected_repository_falls_back_to_the_top() {
    let mut app = app_with(vec![row("alpha", 1), row("beta", 1)]);
    app.on_key(code(KeyCode::Down));

    app.begin_scan();
    app.push_status(row("alpha", 1));
    app.finish_scan();

    assert_eq!(app.selected_index(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib rescan_keeps_the_cursor`
Expected: FAIL — selection is 0 after the rescan, not the index of `gamma`.

- [ ] **Step 3: Preserve the selection across a rescan**

In `src/ui/state.rs`, add the field `pending_selection: Option<PathBuf>` (init `None`), then:

```rust
    pub fn begin_scan(&mut self) {
        self.pending_selection = self.selected().map(|r| r.path.clone());
        if let Some(id) = self.scan_job.take() {
            self.end_job(id);
        }
        self.scan_job = Some(self.begin_job("scanning", 0));
        self.rows.clear();
        self.problems.clear();
        self.selected = 0;
        self.anchor = None;
    }

    pub fn finish_scan(&mut self) {
        if let Some(id) = self.scan_job.take() {
            self.end_job(id);
        }
        let present: BTreeSet<&PathBuf> = self.rows.iter().map(|r| &r.path).collect();
        self.marked.retain(|p| present.contains(p));
        // A rescan is a refresh: the cursor belongs to a repository, not an index.
        if let Some(path) = self.pending_selection.take() {
            if let Some(i) = self.visible().iter().position(|r| r.path == path) {
                self.selected = i;
            }
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib rescan`
Expected: PASS (both tests).

- [ ] **Step 5: Wire the event loop**

In `src/ui/app.rs`, add to `Msg`:

```rust
    JobStep(u64),
    JobEnd(u64),
```

Add to `LoopState`:

```rust
    /// The job whose completion owes a full rescan, set by fetch-all.
    rescan_after: Option<u64>,
```

(initialised `rescan_after: None` in `run`).

Extract the rescan so both callers share it:

```rust
fn do_rescan(app: &mut App, ls: &mut LoopState, opts: &ScanOptions, tx: &mpsc::Sender<Msg>) {
    app.begin_scan();
    ls.current_detail = None;
    ls.generation += 1;
    spawn_scan(opts.clone(), ls.generation, tx.clone());
    request_current_detail(app, ls.generation, tx);
}
```

and make `Command::Rescan => do_rescan(app, ls, opts, tx),`.

Handle the new messages in the drain loop:

```rust
                Msg::JobStep(id) => app.advance_job(id),
                Msg::JobEnd(id) => {
                    app.end_job(id);
                    if ls.rescan_after == Some(id) {
                        ls.rescan_after = None;
                        do_rescan(app, ls, opts, tx);
                    }
                }
```

`Msg::JobEnd` handling calls `do_rescan`, which needs `&mut App` and `&mut LoopState` — the drain loop already holds both, but move the drain into a small helper if the borrow checker objects to `app` being borrowed by the `while let` guard. It does not: `rx.try_recv()` borrows only `rx`.

Reload the selected row's detail whenever its status is refreshed, so a switch or a drop updates the pane without a keypress:

```rust
                Msg::Refresh(status, text) => {
                    let path = status.path.clone();
                    app.push_status(status);
                    if !text.is_empty() {
                        app.set_toast(text, TOAST_TTL);
                    }
                    if app.selected().is_some_and(|s| s.path == path) {
                        spawn_detail(path, ls.generation, tx.clone());
                    }
                }
```

Rewrite the two fetch arms. The "fetching N…" toasts go away — the spinner says it better and keeps saying it:

```rust
            Command::Fetch(paths) => {
                let targets = named(app, paths);
                let job = app.begin_job("fetching", targets.len());
                spawn_fetch(targets, cfg.fetch_concurrency, job, tx.clone());
            }
            Command::FetchAll => {
                let paths: Vec<PathBuf> = app.visible().iter().map(|r| r.path.clone()).collect();
                let targets = named(app, paths);
                let job = app.begin_job("fetching", targets.len());
                ls.rescan_after = Some(job);
                spawn_fetch(targets, cfg.fetch_concurrency, job, tx.clone());
            }
```

and the prune arm:

```rust
            Command::Prune(paths) => {
                let targets = named(app, paths);
                let job = app.begin_job("pruning", targets.len());
                spawn_worktree_op(targets, actions::prune_gone_branches, "pruned", job, tx.clone());
            }
```

Delete the now-unused `plural` helper if nothing else calls it (`cargo clippy -D warnings` will tell you).

Rewrite `spawn_fetch` to carry names, report progress, and refresh each row as it lands:

```rust
/// Fetch in the background, never more than `concurrency` at a time — 500
/// simultaneous SSH sessions would be worse than useless. Each repository is
/// re-inspected as its fetch lands, so the list updates during the sweep.
fn spawn_fetch(
    targets: Vec<(PathBuf, String)>,
    concurrency: usize,
    job: u64,
    tx: mpsc::Sender<Msg>,
) {
    std::thread::spawn(move || {
        let total = targets.len();
        let (work_tx, work_rx) = crossbeam_channel::unbounded::<(PathBuf, String)>();
        for t in targets {
            let _ = work_tx.send(t);
        }
        drop(work_tx);

        let failures = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..concurrency.max(1))
            .map(|_| {
                let rx = work_rx.clone();
                let tx = tx.clone();
                let failures = Arc::clone(&failures);
                std::thread::spawn(move || {
                    for (path, name) in rx {
                        match actions::fetch(&path) {
                            Ok(_) => {
                                let _ = tx.send(Msg::Refresh(
                                    status::inspect(&path, name),
                                    String::new(),
                                ));
                            }
                            Err(_) => {
                                failures.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        let _ = tx.send(Msg::JobStep(job));
                    }
                })
            })
            .collect();
        drop(work_rx);
        for h in handles {
            let _ = h.join();
        }

        let failed = failures.load(Ordering::Relaxed);
        let text = if failed == 0 {
            format!("fetched {total} repositories")
        } else {
            format!("fetched {}/{total} ({failed} failed)", total - failed)
        };
        let _ = tx.send(Msg::ActionDone(text));
        let _ = tx.send(Msg::JobEnd(job));
    });
}
```

Give `spawn_worktree_op` the same treatment: add a `job: u64` parameter, send `Msg::JobStep(job)` after each repository, and `Msg::JobEnd(job)` last.

- [ ] **Step 6: Run the suite**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings (including no dead-code warning for `plural`).

- [ ] **Step 7: Verify the regression tests are not vacuous**

```bash
S=/tmp/claude-1000/-home-rquinaud/7c47e968-fbbc-4968-a93e-bcdee3a7d43e/scratchpad
cp src/ui/state.rs $S/state.bak
# revert only the selection restore in finish_scan, then:
cargo test --lib rescan     # expect FAIL
cp $S/state.bak src/ui/state.rs
cargo test --lib rescan     # expect PASS
```

Never use `git checkout` for this.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: fetch refreshes rows as it goes, and F rescans at the end

Each row is re-inspected as its own fetch lands, so the list is never
stale mid-sweep. Fetch-all then rescans to catch repos cloned or removed
since startup, and a rescan now keeps the cursor on the same repository
instead of throwing it to the top.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 5: Symmetric range marks

**Files:**
- Modify: `src/ui/state.rs` (`anchor`, `toggle_mark`, `mark_range`)
- Modify: `src/ui/view.rs` (`draw_help`), `README.md`
- Test: `src/ui/state.rs` tests

**Interfaces:**
- Consumes: Task 4.
- Produces: `anchor: Option<(usize, bool)>` — index and whether that row ended up marked.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_range_takes_the_state_the_anchor_landed_in() {
    let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1), row("d", 1)]);
    app.on_key(key('a')); // mark all four
    assert_eq!(app.marked_count(), 4);

    app.on_key(code(KeyCode::Down)); // row b
    app.on_key(key(' '));            // unmarks b, anchor = (1, false)
    app.on_key(code(KeyCode::Down)); // row c
    app.on_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));

    let marked: Vec<&str> = app
        .visible()
        .iter()
        .filter(|r| app.is_marked(&r.path))
        .map(|r| r.display_name.as_str())
        .collect();
    assert_eq!(marked, vec!["a", "d"], "b..c should have been swept clear");
}

#[test]
fn a_range_still_marks_when_the_anchor_was_marked() {
    let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
    app.on_key(key(' '));            // marks a, anchor = (0, true)
    app.on_key(code(KeyCode::Down));
    app.on_key(code(KeyCode::Down)); // row c
    app.on_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
    assert_eq!(app.marked_count(), 3);
}

#[test]
fn a_range_with_no_anchor_marks_only_the_current_row() {
    let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
    app.on_key(code(KeyCode::Down));
    app.on_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
    assert_eq!(app.marked_count(), 1);
    assert!(app.is_marked(&app.visible()[1].path));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib a_range_takes_the_state`
Expected: FAIL — `V` only ever adds marks, so the sweep leaves all four marked.

- [ ] **Step 3: Implement**

Change the field to `anchor: Option<(usize, bool)>`, then:

```rust
    fn toggle_mark(&mut self) {
        if let Some(path) = self.selected_path() {
            let now_marked = !self.marked.remove(&path);
            if now_marked {
                self.marked.insert(path);
            }
            self.anchor = Some((self.selected, now_marked));
        }
    }

    /// Sweep the anchor's own outcome across the range: `Space` on an unmarked
    /// row then `V` marks the sweep, `Space` on a marked row then `V` clears it.
    /// With no anchor it marks the current row only — sweeping in everything
    /// above the cursor would be a lot to do with one keystroke.
    fn mark_range(&mut self) {
        let (anchor, mark) = self.anchor.unwrap_or((self.selected, true));
        let (lo, hi) = if anchor <= self.selected {
            (anchor, self.selected)
        } else {
            (self.selected, anchor)
        };
        let paths: Vec<PathBuf> = self
            .visible()
            .get(lo..=hi)
            .unwrap_or_default()
            .iter()
            .map(|r| r.path.clone())
            .collect();
        for p in paths {
            if mark {
                self.marked.insert(p);
            } else {
                self.marked.remove(&p);
            }
        }
        self.anchor = Some((self.selected, mark));
    }
```

Every other `self.anchor = None;` in the file is unchanged — a stale index refers to a different row once the visible set changes.

Update the help pane row and the README row for `V`:

```rust
        ("V", "sweep the last mark's state to here"),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib range`
Expected: PASS.

- [ ] **Step 5: Verify the tests are not vacuous**

Restore the old one-way `mark_range` from the scratchpad copy, confirm `a_range_takes_the_state_the_anchor_landed_in` FAILS, restore.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: sweep marks off as well as on

V now applies whatever the last Space did to the whole range, so a bulk
selection can be undone the same way it was made.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 6: Collect the incoming diff stat

**Files:**
- Modify: `src/detail.rs`
- Test: `src/detail.rs` (`mod tests`, new), `tests/detail.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct FileDelta { pub added: u32, pub removed: u32, pub path: String }`
  - `pub struct Incoming { pub files: Vec<FileDelta>, pub truncated: bool, pub total_added: u32, pub total_removed: u32, pub range: String }`
  - `pub fn parse_numstat(out: &str) -> Vec<FileDelta>`
  - `RepoDetail.incoming: Option<Incoming>`

- [ ] **Step 1: Write the failing unit tests**

Add to `src/detail.rs` a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_lines_become_deltas() {
        let out = "42\t17\tsrc/api/users.rs\n8\t0\tsrc/db/schema.sql\n";
        assert_eq!(
            parse_numstat(out),
            vec![
                FileDelta { added: 42, removed: 17, path: "src/api/users.rs".into() },
                FileDelta { added: 8, removed: 0, path: "src/db/schema.sql".into() },
            ]
        );
    }

    #[test]
    fn a_binary_file_counts_as_no_lines_either_way() {
        let out = "-\t-\tlogo.png\n";
        assert_eq!(
            parse_numstat(out),
            vec![FileDelta { added: 0, removed: 0, path: "logo.png".into() }]
        );
    }

    #[test]
    fn a_rename_keeps_its_whole_path_text() {
        let out = "1\t1\tsrc/{old.rs => new.rs}\n";
        assert_eq!(parse_numstat(out)[0].path, "src/{old.rs => new.rs}");
    }

    #[test]
    fn empty_output_is_no_files_rather_than_a_failure() {
        assert!(parse_numstat("").is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib numstat`
Expected: FAIL to compile — `parse_numstat` and `FileDelta` do not exist.

- [ ] **Step 3: Implement**

In `src/detail.rs`:

```rust
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDelta {
    pub added: u32,
    pub removed: u32,
    pub path: String,
}

/// What a pull would bring in.
#[derive(Debug, Clone, Default)]
pub struct Incoming {
    pub files: Vec<FileDelta>,
    pub truncated: bool,
    pub total_added: u32,
    pub total_removed: u32,
    pub range: String,
}

/// `git diff --numstat` output: added, removed, path, tab-separated. A binary
/// file reports `-` for both counts.
pub fn parse_numstat(out: &str) -> Vec<FileDelta> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?;
            let removed = fields.next()?;
            let path = fields.next()?;
            Some(FileDelta {
                added: added.parse().unwrap_or(0),
                removed: removed.parse().unwrap_or(0),
                path: path.to_string(),
            })
        })
        .collect()
}

fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Three dots deliberately: against a diverged branch `HEAD..@{u}` describes
/// the wrong thing, while `HEAD...@{u}` is what a pull would actually bring.
fn incoming(path: &Path) -> Option<Incoming> {
    let mut files = parse_numstat(&git_stdout(path, &["diff", "--numstat", "HEAD...@{u}"])?);
    let truncated = files.len() > CHANGED_LIMIT;
    let total_added = files.iter().map(|f| f.added).sum();
    let total_removed = files.iter().map(|f| f.removed).sum();
    files.truncate(CHANGED_LIMIT);

    let revs = git_stdout(path, &["rev-parse", "--short", "HEAD", "@{u}"])?;
    let mut ids = revs.split_whitespace();
    let range = match (ids.next(), ids.next()) {
        (Some(a), Some(b)) => format!("{a}..{b}"),
        _ => String::new(),
    };

    Some(Incoming {
        files,
        truncated,
        total_added,
        total_removed,
        range,
    })
}
```

Add `pub incoming: Option<Incoming>` to `RepoDetail`, and at the end of `inspect`, after `out.branches` is filled:

```rust
    let behind = out
        .branches
        .iter()
        .find(|b| b.is_head)
        .map_or(0, |b| b.behind);
    if behind > 0 {
        out.incoming = incoming(path);
    }
```

- [ ] **Step 4: Write the integration test**

In `tests/detail.rs`:

```rust
#[test]
fn a_repo_that_is_behind_reports_what_is_coming() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    let up = r.with_upstream();
    let _clone = r.advance_upstream(&up, "incoming.txt");
    r.git(&["fetch", "-q", "origin"]);

    let d = inspect(r.path());
    let inc = d.incoming.expect("behind, so there is something incoming");
    assert_eq!(inc.files.len(), 1);
    assert_eq!(inc.files[0].path, "incoming.txt");
    assert_eq!(inc.files[0].added, 1);
    assert!(inc.range.contains(".."), "range was {:?}", inc.range);
}

#[test]
fn an_up_to_date_repo_has_nothing_incoming() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.with_upstream();
    assert!(inspect(r.path()).incoming.is_none());
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test numstat && cargo test --test detail && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: collect the incoming diff stat

One `git diff --numstat HEAD...@{u}`, run only for the selected repo and
only when it is behind. Three dots so a diverged branch reports what a
pull would bring rather than inverting the local commits.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 7: Render the incoming block

**Files:**
- Modify: `src/ui/view.rs` (`draw_detail`, `stat_bar`)
- Modify: `src/theme.rs` (two facets)
- Test: `src/ui/view.rs` tests

**Interfaces:**
- Consumes: Task 6's `Incoming`.
- Produces: `Facet::Added`, `Facet::Removed`; `fn stat_bar(d: &FileDelta, max: u32, width: usize) -> Vec<Span<'static>>`.

- [ ] **Step 1: Write the failing test**

In `src/ui/view.rs` tests:

```rust
#[test]
fn the_widest_file_fills_the_bar_and_the_rest_scale_to_it() {
    let big = FileDelta { added: 40, removed: 0, path: "big".into() };
    let small = FileDelta { added: 10, removed: 0, path: "small".into() };
    let cells = |d| stat_bar(d, 40, 20).iter().map(|s| s.content.chars().count()).sum::<usize>();
    assert_eq!(cells(&big), 20);
    assert_eq!(cells(&small), 5);
}

#[test]
fn a_file_with_no_changed_lines_draws_no_bar() {
    let binary = FileDelta { added: 0, removed: 0, path: "logo.png".into() };
    assert!(stat_bar(&binary, 40, 20).is_empty());
}

#[test]
fn the_detail_pane_lists_what_is_incoming() {
    let app = app_with(vec![row("repo", 1)]);
    let detail = RepoDetail {
        incoming: Some(Incoming {
            files: vec![FileDelta { added: 42, removed: 17, path: "src/api/users.rs".into() }],
            truncated: false,
            total_added: 42,
            total_removed: 17,
            range: "abc1234..def5678".into(),
        }),
        ..RepoDetail::default()
    };
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let mut state = ListState::default();
    term.draw(|f| draw(f, &app, Some(&detail), &mut state)).unwrap();
    let out = buffer_string(term.backend().buffer());

    assert!(out.contains("abc1234..def5678"), "{out}");
    assert!(out.contains("src/api/users.rs"), "{out}");
    assert!(out.contains("1 file, 42 insertions(+), 17 deletions(-)"), "{out}");
    assert!(out.contains("D diff"), "{out}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib incoming`
Expected: FAIL to compile — `stat_bar` and `Incoming` are not in scope in `view.rs`.

- [ ] **Step 3: Add the facets**

In `src/theme.rs`, add `Added` and `Removed` to `Facet`, to `Facet::ALL` (bump the array length to 14), and give each a colour and glyph in the existing `match` arms — `Added` green with glyph `+`, `Removed` red with glyph `-`. Follow whatever shape the existing arms use; the theme test that walks `Facet::ALL` will fail until every arm is filled in, which is the point.

- [ ] **Step 4: Implement the renderer**

In `src/ui/view.rs`, add `use crate::detail::{FileDelta, Incoming};` (alongside the existing `RepoDetail` import) and:

```rust
/// A `+++---` bar scaled so the widest file in the set fills `width`.
fn stat_bar(d: &FileDelta, max: u32, width: usize) -> Vec<Span<'static>> {
    let total = d.added + d.removed;
    if total == 0 || max == 0 {
        return Vec::new();
    }
    let cells = ((total as usize * width) / max as usize).max(1);
    let plus = (d.added as usize * cells) / total as usize;
    let minus = cells - plus;
    let mut spans = Vec::new();
    if plus > 0 {
        spans.push(Span::styled("+".repeat(plus), facet_style(Facet::Added)));
    }
    if minus > 0 {
        spans.push(Span::styled("-".repeat(minus), facet_style(Facet::Removed)));
    }
    spans
}
```

In `draw_detail`, immediately after the `Branches` loop:

```rust
    if let Some(inc) = &d.incoming {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Incoming  ", dim()),
            Span::styled(inc.range.clone(), accent()),
        ]));
        let name_w = inc
            .files
            .iter()
            .map(|f| f.path.chars().count())
            .max()
            .unwrap_or(0)
            .min(40);
        let max = inc
            .files
            .iter()
            .map(|f| f.added + f.removed)
            .max()
            .unwrap_or(0);
        for f in &inc.files {
            let mut spans = vec![
                Span::raw("  "),
                Span::raw(format!("{:name_w$} ", elide(&f.path, name_w))),
            ];
            spans.extend(stat_bar(f, max, 20));
            lines.push(Line::from(spans));
        }
        if inc.truncated {
            lines.push(Line::from(Span::styled("  …", dim())));
        }
        lines.push(Line::from(Span::styled(
            format!(
                "  {} file{}, {} insertions(+), {} deletions(-)",
                inc.files.len(),
                if inc.files.len() == 1 { "" } else { "s" },
                inc.total_added,
                inc.total_removed
            ),
            dim(),
        )));
        lines.push(Line::from(Span::styled("  D diff   L log", accent())));
    }
```

`elide` comes from `crate::render`; it is already imported in `view.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: show what a pull would bring

The detail pane lists the incoming files with scaled +/- bars and the
commit range, the way `git pull` reports one. The characters carry the
meaning, so it reads the same with NO_COLOR.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 8: Hand the diff to the user's pager — `D` and `L`

**Files:**
- Modify: `src/actions.rs`, `src/ui/state.rs`, `src/ui/app.rs`, `src/ui/view.rs`, `README.md`
- Test: `src/ui/state.rs` tests

**Interfaces:**
- Consumes: Task 7.
- Produces:
  - `actions::view_diff(path: &Path) -> Result<(), ActionError>`
  - `actions::view_log(path: &Path) -> Result<(), ActionError>`
  - `Command::Diff(PathBuf)`, `Command::Log(PathBuf)`

- [ ] **Step 1: Write the failing tests**

In `src/ui/state.rs` tests (add a `row_behind` helper next to `row`):

```rust
fn row_behind(name: &str) -> RepoStatus {
    RepoStatus { behind: 3, ..row(name, 0) }
}

fn row_no_upstream(name: &str) -> RepoStatus {
    RepoStatus { upstream: None, ..row(name, 0) }
}

#[test]
fn d_and_l_open_the_pager_for_the_selected_repository() {
    let mut app = app_with(vec![row_behind("a")]);
    let path = app.selected().unwrap().path.clone();
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)),
        Command::Diff(path.clone())
    );
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT)),
        Command::Log(path)
    );
}

#[test]
fn there_is_nothing_to_diff_against_without_an_upstream() {
    let mut app = app_with(vec![row_no_upstream("a")]);
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)),
        Command::None
    );
    assert!(app.toast().unwrap().contains("upstream"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib pager`
Expected: FAIL to compile — `Command::Diff` does not exist.

- [ ] **Step 3: Implement the actions**

In `src/actions.rs`:

```rust
/// Inherits the terminal, so git sees a tty and invokes the user's pager with
/// the user's own configuration. The caller must leave raw mode first.
fn run_git_interactive(path: &Path, args: &[&str]) -> Result<(), ActionError> {
    run_interactive(path, "git", args)
}

pub fn view_diff(path: &Path) -> Result<(), ActionError> {
    run_git_interactive(path, &["diff", "HEAD...@{u}"])
}

/// Two dots: the commits that are incoming, not a symmetric difference.
pub fn view_log(path: &Path) -> Result<(), ActionError> {
    run_git_interactive(path, &["log", "--stat", "HEAD..@{u}"])
}
```

- [ ] **Step 4: Implement the keys**

In `src/ui/state.rs`, add `Diff(PathBuf)` and `Log(PathBuf)` to `Command`, then a helper and two arms:

```rust
    /// The selected repository, if it has something to compare against.
    fn upstream_target(&mut self) -> Option<PathBuf> {
        match self.selected() {
            Some(r) if r.upstream.is_some() => Some(r.path.clone()),
            Some(_) => {
                self.set_toast("branch has no upstream", Duration::from_secs(6));
                None
            }
            None => None,
        }
    }
```

```rust
            KeyCode::Char('D') => self.upstream_target().map_or(Command::None, Command::Diff),
            KeyCode::Char('L') => self.upstream_target().map_or(Command::None, Command::Log),
```

In `src/ui/app.rs`, next to the existing `Command::Shell` arm:

```rust
            Command::Diff(path) => {
                if let Err(e) = suspend(term, || actions::view_diff(&path))? {
                    app.set_toast(e.to_string(), TOAST_TTL);
                }
            }
            Command::Log(path) => {
                if let Err(e) = suspend(term, || actions::view_log(&path))? {
                    app.set_toast(e.to_string(), TOAST_TTL);
                }
            }
```

In `src/ui/view.rs`, add to `HINTS` and `draw_help`:

```rust
    ("D", "diff"), ("L", "log"),
```
```rust
        ("D", "diff HEAD...@{u} in your pager"),
        ("L", "log of the incoming commits"),
```

Add both rows to the README keybinding table.

- [ ] **Step 5: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: D and L open the incoming diff in your pager

suspend() hands the terminal back to git, so core.pager, delta and every
colour setting apply untouched. gitdrift renders no diffs of its own.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 9: Branch switcher — `b`

**Files:**
- Modify: `src/ui/state.rs`, `src/ui/view.rs`, `src/ui/app.rs`, `src/actions.rs`, `README.md`
- Test: `src/ui/state.rs` tests, `tests/actions.rs`

**Interfaces:**
- Consumes: Tasks 2 (`Picker`, `draw_picker`) and 4 (`Msg::Refresh` reloads detail).
- Produces:
  - `pub struct BranchChoice { pub name: String, pub ahead: u32, pub behind: u32, pub has_upstream: bool, pub is_head: bool }`
  - `Pane::Branches`, `Command::OpenBranches`, `Command::Checkout(PathBuf, String)`
  - `App::open_branch_picker(&mut self, rows: Vec<BranchChoice>)`, `App::branch_choices(&self) -> &[BranchChoice]`
  - `actions::switch_branch(path: &Path, branch: &str) -> Result<String, ActionError>`

- [ ] **Step 1: Write the failing tests**

```rust
fn branch(name: &str, is_head: bool) -> BranchChoice {
    BranchChoice { name: name.into(), ahead: 0, behind: 0, has_upstream: true, is_head }
}

#[test]
fn b_refuses_to_switch_with_uncommitted_work() {
    let mut app = app_with(vec![RepoStatus { unstaged: 2, ..row("a", 0) }]);
    assert_eq!(app.on_key(key('b')), Command::None);
    assert_eq!(app.pane(), Pane::List);
    assert!(app.toast().unwrap().contains("uncommitted"));
}

#[test]
fn b_asks_the_loop_for_the_branch_list() {
    let mut app = app_with(vec![row("a", 0)]);
    assert_eq!(app.on_key(key('b')), Command::OpenBranches);
}

#[test]
fn choosing_a_branch_checks_it_out() {
    let mut app = app_with(vec![row("a", 0)]);
    let path = app.selected().unwrap().path.clone();
    app.open_branch_picker(vec![branch("main", true), branch("feat/retries", false)]);
    assert_eq!(app.pane(), Pane::Branches);
    app.on_key(code(KeyCode::Down));
    assert_eq!(
        app.on_key(code(KeyCode::Enter)),
        Command::Checkout(path, "feat/retries".into())
    );
    assert_eq!(app.pane(), Pane::List);
}

#[test]
fn choosing_the_branch_already_checked_out_does_nothing() {
    let mut app = app_with(vec![row("a", 0)]);
    app.open_branch_picker(vec![branch("main", true)]);
    assert_eq!(app.on_key(code(KeyCode::Enter)), Command::None);
    assert_eq!(app.pane(), Pane::List);
}

#[test]
fn esc_leaves_the_branch_picker_without_switching() {
    let mut app = app_with(vec![row("a", 0)]);
    app.open_branch_picker(vec![branch("main", true), branch("other", false)]);
    app.on_key(code(KeyCode::Down));
    assert_eq!(app.on_key(code(KeyCode::Esc)), Command::None);
    assert_eq!(app.pane(), Pane::List);
}
```

In `tests/actions.rs`:

```rust
#[test]
fn switch_branch_moves_head() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.git(&["branch", "other"]);
    switch_branch(r.path(), "other").unwrap();
    assert_eq!(inspect(r.path(), "fixture".into()).head, Head::Branch("other".into()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib branch`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the action**

In `src/actions.rs`:

```rust
/// Move HEAD to an existing local branch. Non-destructive: git refuses rather
/// than losing work, and the caller has already rejected a dirty worktree.
pub fn switch_branch(path: &Path, branch: &str) -> Result<String, ActionError> {
    run_git(path, &["switch", branch])
}
```

- [ ] **Step 4: Implement the pane**

In `src/ui/state.rs`:

```rust
/// One row of the branch picker. Built by the event loop from the loaded
/// detail, so the state machine stays free of `crate::detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchChoice {
    pub name: String,
    pub ahead: u32,
    pub behind: u32,
    pub has_upstream: bool,
    pub is_head: bool,
}
```

Add `Branches` to `Pane`, `OpenBranches` and `Checkout(PathBuf, String)` to `Command`, and `branch_choices: Vec<BranchChoice>` to `App` (init empty).

```rust
    pub fn open_branch_picker(&mut self, rows: Vec<BranchChoice>) {
        let at = rows.iter().position(|b| b.is_head).unwrap_or(0);
        self.picker = Some(Picker::open(rows.len(), at));
        self.branch_choices = rows;
        self.pane = Pane::Branches;
    }

    pub fn branch_choices(&self) -> &[BranchChoice] {
        &self.branch_choices
    }

    fn choose_branch(&mut self) -> Command {
        let choice = self.branch_choices.get(self.picker_index()).cloned();
        self.close_picker();
        match (choice, self.selected_path()) {
            (Some(c), Some(p)) if !c.is_head => Command::Checkout(p, c.name),
            _ => Command::None,
        }
    }

    fn close_picker(&mut self) {
        self.pane = Pane::List;
        self.picker = None;
    }
```

The list keymap arm:

```rust
            KeyCode::Char('b') => match self.selected() {
                Some(r) if r.is_dirty() => {
                    self.set_toast(
                        "worktree has uncommitted changes",
                        Duration::from_secs(6),
                    );
                    Command::None
                }
                Some(_) => Command::OpenBranches,
                None => Command::None,
            },
```

In `on_key_overlay`, add a branch alongside the namespace one:

```rust
        if self.pane == Pane::Branches {
            match key.code {
                KeyCode::Down => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_by(1);
                    }
                    return Command::None;
                }
                KeyCode::Up => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_by(-1);
                    }
                    return Command::None;
                }
                KeyCode::Enter => return self.choose_branch(),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('b') => {
                    self.close_picker();
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
```

Migrate the namespace branch to use `close_picker()` too, so the three panes close identically.

- [ ] **Step 5: Render it**

In `src/ui/view.rs`, add to the `match app.pane()` in `draw`:

```rust
        Pane::Branches => {
            draw_list(frame, app, body[0], list_state);
            draw_branches(frame, app, body[0]);
        }
```

and:

```rust
fn draw_branches(frame: &mut Frame, app: &App, area: Rect) {
    let name_w = app
        .branch_choices()
        .iter()
        .map(|b| b.name.chars().count())
        .max()
        .unwrap_or(6);

    let rows: Vec<Line<'static>> = app
        .branch_choices()
        .iter()
        .map(|b| {
            let mut spans = vec![
                Span::styled(if b.is_head { "● " } else { "  " }, accent()),
                Span::raw(format!("{:name_w$}", b.name)),
            ];
            if b.ahead > 0 {
                spans.push(Span::styled(format!(" ↑{}", b.ahead), facet_style(Facet::Ahead)));
            }
            if b.behind > 0 {
                spans.push(Span::styled(format!(" ↓{}", b.behind), facet_style(Facet::Behind)));
            }
            if !b.has_upstream {
                spans.push(Span::styled(" ⊘", facet_style(Facet::NoUpstream)));
            }
            Line::from(spans)
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Switch branch",
        rows,
        app.picker_index(),
        "↑↓ move   Enter switch   Esc close",
    );
}
```

Add `("b", "branch")` to `HINTS` and `("b", "switch branch on the current repo")` to `draw_help`, plus a README row.

- [ ] **Step 6: Wire the loop**

In `src/ui/app.rs`:

```rust
            Command::OpenBranches => match ls.current_detail.as_ref() {
                Some((_, d)) => app.open_branch_picker(
                    d.branches
                        .iter()
                        .map(|b| BranchChoice {
                            name: b.name.clone(),
                            ahead: b.ahead,
                            behind: b.behind,
                            has_upstream: b.upstream.is_some(),
                            is_head: b.is_head,
                        })
                        .collect(),
                ),
                None => app.set_toast("still loading…", TOAST_TTL),
            },
            Command::Checkout(path, branch) => {
                let name = display_name_of(app, &path);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let msg = match actions::switch_branch(&path, &branch) {
                        Ok(_) => Msg::Refresh(
                            status::inspect(&path, name),
                            format!("switched to {branch}"),
                        ),
                        Err(e) => Msg::ActionDone(format!("{name}: {e}")),
                    };
                    let _ = tx.send(msg);
                });
            }
```

Import `BranchChoice` from `crate::ui::state`.

- [ ] **Step 7: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: switch branch from the list with b

Local branches only, on the selected repo, refused on a dirty worktree.
The picker rows come from the detail already loaded, so nothing new is
gathered and the state machine stays free of detail.rs.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 10: Stash browser — `S`

**Files:**
- Modify: `src/ui/state.rs`, `src/ui/view.rs`, `src/ui/app.rs`, `src/actions.rs`, `README.md`
- Test: `src/ui/state.rs` tests, `tests/actions.rs`

**Interfaces:**
- Consumes: Tasks 2 and 9 (`close_picker`, `draw_picker`).
- Produces:
  - `pub struct StashChoice { pub index: usize, pub message: String }`
  - `Pane::Stashes`, `Command::OpenStashes`, `Command::ShowStash(PathBuf, usize)`, `Command::DropStash(PathBuf, usize)`
  - `Destructive::StashDrop`; `Pending` gains `pub stash: Option<usize>`
  - `actions::view_stash(path: &Path, index: usize) -> Result<(), ActionError>`
  - `actions::drop_stash(path: &Path, index: usize) -> Result<String, ActionError>`

- [ ] **Step 1: Write the failing tests**

```rust
fn stash(index: usize, message: &str) -> StashChoice {
    StashChoice { index, message: message.into() }
}

#[test]
fn enter_views_a_stash_in_the_pager() {
    let mut app = app_with(vec![row("a", 0)]);
    let path = app.selected().unwrap().path.clone();
    app.open_stash_picker(vec![stash(0, "wip"), stash(1, "other")]);
    app.on_key(code(KeyCode::Down));
    assert_eq!(app.on_key(code(KeyCode::Enter)), Command::ShowStash(path, 1));
}

#[test]
fn dropping_a_stash_asks_first() {
    let mut app = app_with(vec![row("a", 0)]);
    app.open_stash_picker(vec![stash(0, "wip: retry backoff")]);
    assert_eq!(app.on_key(key('x')), Command::None);
    assert_eq!(app.pane(), Pane::Confirm);
    let pending = app.pending().unwrap();
    assert_eq!(pending.stash, Some(0));
    assert!(pending.summary.contains("wip: retry backoff"));
}

#[test]
fn only_lowercase_y_drops_the_stash() {
    let mut app = app_with(vec![row("a", 0)]);
    let path = app.selected().unwrap().path.clone();
    app.open_stash_picker(vec![stash(0, "wip")]);
    app.on_key(key('x'));
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
        Command::None,
        "the pane promises any other key cancels"
    );

    app.open_stash_picker(vec![stash(0, "wip")]);
    app.on_key(key('x'));
    assert_eq!(app.on_key(key('y')), Command::DropStash(path, 0));
}

#[test]
fn s_asks_the_loop_for_the_stash_list() {
    let mut app = app_with(vec![row("a", 0)]);
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT)),
        Command::OpenStashes
    );
}

#[test]
fn an_empty_stash_list_opens_nothing() {
    let mut app = app_with(vec![row("a", 0)]);
    app.open_stash_picker(Vec::new());
    assert_eq!(app.pane(), Pane::List);
    assert!(app.toast().unwrap().contains("no stashes"));
}
```

In `tests/actions.rs`:

```rust
#[test]
fn drop_stash_removes_exactly_one_entry() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.write("a", "2");
    r.git(&["stash", "push", "-qm", "first"]);
    r.write("a", "3");
    r.git(&["stash", "push", "-qm", "second"]);
    assert_eq!(inspect(r.path(), "fixture".into()).stash_count, 2);

    drop_stash(r.path(), 0).unwrap();
    assert_eq!(inspect(r.path(), "fixture".into()).stash_count, 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib stash`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the actions**

In `src/actions.rs`:

```rust
pub fn view_stash(path: &Path, index: usize) -> Result<(), ActionError> {
    run_git_interactive(path, &["stash", "show", "-p", &stash_ref(index)])
}

/// Irreversible: the caller must have confirmed with the user first. The output
/// names the dropped commit, which is the only way back via `git stash store`.
pub fn drop_stash(path: &Path, index: usize) -> Result<String, ActionError> {
    run_git(path, &["stash", "drop", &stash_ref(index)])
}

fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}
```

- [ ] **Step 4: Implement the pane**

In `src/ui/state.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashChoice {
    pub index: usize,
    pub message: String,
}
```

Add `Stashes` to `Pane`; `OpenStashes`, `ShowStash(PathBuf, usize)`, `DropStash(PathBuf, usize)` to `Command`; `StashDrop` to `Destructive`; `stash_choices: Vec<StashChoice>` to `App`.

`Pending` gains the field — the stash index does not belong in `targets`, which stays a list of repository paths:

```rust
pub struct Pending {
    pub kind: Destructive,
    pub targets: Vec<PathBuf>,
    /// The stash being dropped, for `Destructive::StashDrop` only.
    pub stash: Option<usize>,
    pub summary: String,
}
```

Set `stash: None` in the existing `confirm()`. Extend `Destructive::verb`:

```rust
            Destructive::StashDrop => "drop",
```

```rust
    pub fn open_stash_picker(&mut self, rows: Vec<StashChoice>) {
        if rows.is_empty() {
            self.set_toast("no stashes", Duration::from_secs(6));
            return;
        }
        self.picker = Some(Picker::open(rows.len(), 0));
        self.stash_choices = rows;
        self.pane = Pane::Stashes;
    }

    pub fn stash_choices(&self) -> &[StashChoice] {
        &self.stash_choices
    }

    fn confirm_stash_drop(&mut self) -> Command {
        let (Some(choice), Some(path)) = (
            self.stash_choices.get(self.picker_index()).cloned(),
            self.selected_path(),
        ) else {
            return Command::None;
        };
        self.close_picker();
        self.pending = Some(Pending {
            kind: Destructive::StashDrop,
            targets: vec![path],
            stash: Some(choice.index),
            summary: format!(
                "drop stash@{{{}}} — \"{}\"",
                choice.index, choice.message
            ),
        });
        self.pane = Pane::Confirm;
        Command::None
    }
```

List keymap:

```rust
            KeyCode::Char('S') => Command::OpenStashes,
```

Overlay branch:

```rust
        if self.pane == Pane::Stashes {
            match key.code {
                KeyCode::Down => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_by(1);
                    }
                    return Command::None;
                }
                KeyCode::Up => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_by(-1);
                    }
                    return Command::None;
                }
                KeyCode::Enter => {
                    let index = self.stash_choices.get(self.picker_index()).map(|s| s.index);
                    let path = self.selected_path();
                    self.close_picker();
                    return match (path, index) {
                        (Some(p), Some(i)) => Command::ShowStash(p, i),
                        _ => Command::None,
                    };
                }
                KeyCode::Char('x') => return self.confirm_stash_drop(),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('S') => {
                    self.close_picker();
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
```

Confirm branch gains the new kind:

```rust
                (true, Some(p)) => match (p.kind, p.stash) {
                    (Destructive::Prune, _) => Command::Prune(p.targets),
                    (Destructive::StashDrop, Some(i)) => {
                        match p.targets.into_iter().next() {
                            Some(path) => Command::DropStash(path, i),
                            None => Command::None,
                        }
                    }
                    (Destructive::StashDrop, None) => Command::None,
                },
```

- [ ] **Step 5: Render it**

```rust
fn draw_stashes(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Line<'static>> = app
        .stash_choices()
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled(format!("  ⚑{:<3}", s.index), facet_style(Facet::Stashed)),
                Span::raw(elide(&s.message, 60)),
            ])
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Stashes",
        rows,
        app.picker_index(),
        "↑↓ move   Enter view   x drop   Esc close",
    );
}
```

Add the `Pane::Stashes` arm to `draw` (list first, popover over it), `("S", "stash")` to `HINTS`, `("S", "browse stashes: view, drop")` to `draw_help`, and a README row.

- [ ] **Step 6: Wire the loop**

```rust
            Command::OpenStashes => match ls.current_detail.as_ref() {
                Some((_, d)) => app.open_stash_picker(
                    d.stashes
                        .iter()
                        .map(|s| StashChoice {
                            index: s.index,
                            message: s.message.clone(),
                        })
                        .collect(),
                ),
                None => app.set_toast("still loading…", TOAST_TTL),
            },
            Command::ShowStash(path, index) => {
                if let Err(e) = suspend(term, || actions::view_stash(&path, index))? {
                    app.set_toast(e.to_string(), TOAST_TTL);
                }
            }
            Command::DropStash(path, index) => {
                let name = display_name_of(app, &path);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let msg = match actions::drop_stash(&path, index) {
                        // git prints the dropped commit id; carrying it in the
                        // toast is the only way back, via `git stash store`.
                        Ok(out) => Msg::Refresh(status::inspect(&path, name), out),
                        Err(e) => Msg::ActionDone(format!("{name}: {e}")),
                    };
                    let _ = tx.send(msg);
                });
            }
```

`Msg::Refresh` already re-requests detail for the selected row (Task 4), which is what renumbers the remaining stashes.

- [ ] **Step 7: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 8: Verify the destructive path is guarded**

Restore `src/ui/state.rs` from a scratchpad copy with the confirm gate removed (`x` returning `Command::DropStash` directly) and confirm `dropping_a_stash_asks_first` FAILS. Restore.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
feat: browse stashes with S

View one in your pager, drop one behind the confirm pane. The drop toast
carries the commit id git prints, which is the only way back. No apply or
pop — s opens a shell where those belong.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 11: Documentation sweep and keymap audit

**Files:**
- Modify: `README.md`, `src/ui/view.rs` (`draw_help`, `HINTS`)
- Test: `src/ui/view.rs` tests

**Interfaces:**
- Consumes: every preceding task.
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_footer_hint_is_a_key_the_help_pane_explains() {
    // Help rows describe key groups: "g / G", "f / F", "\u2191 \u2193 PgUp PgDn".
    let helped: Vec<&str> = HELP
        .iter()
        .flat_map(|(keys, _)| keys.split([' ', '/']))
        .filter(|k| !k.is_empty())
        .collect();
    for (key, _) in HINTS {
        assert!(
            helped.contains(key),
            "the footer offers {key} but the help pane never explains it"
        );
    }
}
```

This requires `draw_help`'s local `bindings` array to become a module-level constant both the renderer and the test can read:

```rust
const HELP: &[(&str, &str)] = &[ /* the existing rows, unchanged */ ];
```

`draw_help` then iterates `HELP` instead of its own local array.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib every_footer_hint`
Expected: FAIL to compile — `help_bindings` does not exist.

- [ ] **Step 3: Implement**

Lift the `bindings` array out of `draw_help` into `help_bindings()` and have `draw_help` call it. Fix any key the test then reports as unexplained.

- [ ] **Step 4: Audit the keymap by hand**

Read the complete list keymap and every overlay branch in `src/ui/state.rs` and confirm:
- No `Char('x') | Char('X')`-style alias pairs anywhere.
- No key bound twice within one pane.
- `x` is bound **only** inside the stash pane.
- The Ctrl-C check and the modifier guard are still the first two statements of `on_key`.

Record what you checked in the commit message.

- [ ] **Step 5: Update the README**

The keybinding table must list, and only list: `↑↓ PgUp PgDn`, `g`/`G`, `n`, `o`, `d`, `/`, `!`, `?`, `r`, `Space`, `V`, `a`, `f`/`F`, `p`, `P`, `D`, `L`, `b`, `S`, `s`, `e`, `q`/`Esc`, and `x` noted as stash-pane-only. Update the prose describing actions to drop discard/clean and mention the pager handoff.

- [ ] **Step 6: Run the tests**

Run: `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "$(cat <<'MSG'
docs: bring the help pane, footer and README back in step

A test now fails if the footer offers a key the help pane does not
explain.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

## Manual verification (needs a tty; the agent cannot do this)

After Task 11, `cargo install --path . --force` and check by hand:

1. `F` — the spinner animates and counts up, rows update as fetches land, a full rescan follows, and the cursor stays on the repository it was on.
2. `Space` then `V` on a marked row clears the sweep; on an unmarked row, marks it.
3. `D` on a repo that is behind — your pager opens with your usual diff colouring; `q` returns to a clean TUI.
4. `L` on the same repo shows the incoming commits.
5. `b` — the picker lists local branches; `Enter` switches and the row updates; `b` on a dirty repo is refused with a toast.
6. `S` — the picker lists stashes; `Enter` shows one in the pager; `x` then `y` drops it and the toast names the dropped commit.
7. `X` and `C` do nothing.
8. `Esc`, `q` and `Ctrl-C` all leave the terminal sane.
