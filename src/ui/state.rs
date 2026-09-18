use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::discover::Problem;
use crate::drift;
use crate::status::RepoStatus;
use crate::ui::picker::Picker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    None,
    Quit,
    /// Network and worktree actions apply to the marked set, or to the
    /// selected row when nothing is marked.
    Fetch(Vec<PathBuf>),
    FetchAll,
    Pull(Vec<PathBuf>),
    /// Prune remote refs, then delete local branches whose upstream is gone.
    /// Only ever reached via a confirmation.
    Prune(Vec<PathBuf>),
    Shell(PathBuf),
    Editor(PathBuf),
    Diff(PathBuf),
    Log(PathBuf),
    /// Diff against the repository's default branch, resolved by the caller
    /// from the loaded detail — the state machine has no branch list of its own.
    AncestorDiff(PathBuf),
    Rescan,
    LoadDetail(PathBuf),
    OpenBranches,
    Checkout(PathBuf, String),
    /// Diff a branch from the picker against the default branch, without
    /// checking it out first.
    BranchDiff(PathBuf, String),
    /// Only ever reached via a confirmation.
    DeleteBranch(PathBuf, String),
    OpenStashes,
    ShowStash(PathBuf, usize),
    DropStash(PathBuf, usize),
}

/// A destructive action waiting for the user to confirm it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub kind: Destructive,
    pub targets: Vec<PathBuf>,
    /// The stash being dropped, for `Destructive::StashDrop` only.
    pub stash: Option<usize>,
    /// The branch being deleted, for `Destructive::BranchDelete` only.
    pub branch: Option<String>,
    /// What the user is about to lose, already counted up for display.
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destructive {
    Prune,
    StashDrop,
    BranchDelete,
}

impl Destructive {
    pub fn verb(self) -> &'static str {
        match self {
            Destructive::Prune => "prune remotes and delete branches whose upstream is gone in",
            Destructive::StashDrop => "drop",
            Destructive::BranchDelete => "delete branch",
        }
    }
}

fn non_empty(v: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    (!v.is_empty()).then_some(v)
}

/// The top-level path segment a repository lives under, e.g. `idp` for
/// `idp/run-plane/foo`. Repositories at a root have an empty namespace.
pub fn namespace(display_name: &str) -> &str {
    display_name.split_once('/').map_or("", |(head, _)| head)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Problems,
    Help,
    Confirm,
    Namespaces,
    Branches,
    Stashes,
    Sort,
}

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

/// One row of the stash picker. Built by the event loop from the loaded
/// detail, so the state machine stays free of `crate::detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashChoice {
    pub index: usize,
    pub message: String,
}

/// Background work in flight. `total == 0` means the size is not yet known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub label: String,
    pub done: usize,
    pub total: usize,
}

pub struct App {
    rows: Vec<RepoStatus>,
    problems: Vec<Problem>,
    selected: usize,
    filter: String,
    filtering: bool,
    drifted_only: bool,
    pane: Pane,
    jobs: Vec<Job>,
    next_job_id: u64,
    /// The scan's job, if one is running. Labels are display text only —
    /// nothing branches on them.
    scan_job: Option<u64>,
    toast: Option<(String, Instant)>,
    marked: BTreeSet<PathBuf>,
    pending: Option<Pending>,
    default_branches: Vec<String>,
    sort: drift::Sort,
    /// The repository the last `Space` landed on, and whether it marked or
    /// unmarked. Held by path so a background re-sort cannot shift it.
    anchor: Option<(PathBuf, bool)>,
    /// Namespace filter, chosen from the `n` picker. `None` shows every one.
    group: Option<String>,
    /// Cursor inside whichever popover is open.
    picker: Option<Picker>,
    /// Rows of the open branch picker, if any.
    branch_choices: Vec<BranchChoice>,
    /// Rows of the open stash picker, if any.
    stash_choices: Vec<StashChoice>,
    /// The repository the open popover describes. Actions taken from it use
    /// this, not the cursor: the cursor can move while the popover is up.
    picker_repo: Option<PathBuf>,
    /// How far the detail pane is scrolled, in lines.
    pane_scroll: u16,
    /// Flips the default visibility `view.rs` picks each frame.
    detail_toggled: bool,
    /// The repository under the cursor when a scan began, restored after it finishes.
    pending_selection: Option<PathBuf>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut app = App {
            rows: Vec::new(),
            problems: Vec::new(),
            selected: 0,
            filter: String::new(),
            filtering: false,
            drifted_only: false,
            pane: Pane::List,
            jobs: Vec::new(),
            next_job_id: 0,
            scan_job: None,
            toast: None,
            marked: BTreeSet::new(),
            pending: None,
            sort: drift::Sort::default(),
            anchor: None,
            group: None,
            picker: None,
            branch_choices: Vec::new(),
            stash_choices: Vec::new(),
            picker_repo: None,
            pane_scroll: 0,
            detail_toggled: false,
            pending_selection: None,
            default_branches: crate::theme::DEFAULT_BRANCHES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        };
        app.scan_job = Some(app.begin_job("scanning", 0));
        app
    }

    // --- ingestion -------------------------------------------------------

    /// Insert or replace a repository's status, keeping the list ordered.
    pub fn push_status(&mut self, s: RepoStatus) {
        match self.rows.iter_mut().find(|r| r.path == s.path) {
            Some(existing) => *existing = s,
            None => self.rows.push(s),
        }
        drift::sort_by(&mut self.rows, self.sort);
        self.clamp_selection();
        if let Some(id) = self.scan_job {
            self.advance_job(id);
        }
    }

    pub fn push_problem(&mut self, p: Problem) {
        self.problems.push(p);
    }

    /// Marks survive a rescan — a rescan is a refresh, not a reset. Marks for
    /// repositories that have gone away are dropped once the scan completes.
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
        if self.picker.is_some() {
            self.close_picker();
        }
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

    pub fn is_scanning(&self) -> bool {
        self.scan_job.is_some()
    }

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

    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// The in-progress filter text, or `None` when not filtering.
    pub fn filter_text(&self) -> Option<&str> {
        self.filtering.then_some(self.filter.as_str())
    }

    // --- selection -------------------------------------------------------

    pub fn visible(&self) -> Vec<&RepoStatus> {
        self.rows
            .iter()
            .filter(|r| !self.drifted_only || drift::is_drifted(r))
            .filter(|r| self.filter.is_empty() || r.display_name.contains(&self.filter))
            .filter(|r| match &self.group {
                Some(g) => namespace(&r.display_name) == g,
                None => true,
            })
            .collect()
    }

    pub fn sort_mode(&self) -> drift::Sort {
        self.sort
    }

    /// The active namespace filter, or `None` when every namespace is shown.
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// Every namespace present, in display order. Ignores the namespace
    /// filter itself, so cycling can always reach the next one.
    fn namespaces(&self) -> Vec<String> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for r in self
            .rows
            .iter()
            .filter(|r| !self.drifted_only || drift::is_drifted(r))
            .filter(|r| self.filter.is_empty() || r.display_name.contains(&self.filter))
        {
            seen.insert(namespace(&r.display_name));
        }
        seen.into_iter().map(str::to_string).collect()
    }

    /// The picker's rows: "all" first, then each namespace, each with the
    /// number of repositories it would show.
    pub fn namespace_choices(&self) -> Vec<(String, usize)> {
        let shown: Vec<&RepoStatus> = self
            .rows
            .iter()
            .filter(|r| !self.drifted_only || drift::is_drifted(r))
            .filter(|r| self.filter.is_empty() || r.display_name.contains(&self.filter))
            .collect();
        let mut out = vec![("all".to_string(), shown.len())];
        for ns in self.namespaces() {
            let count = shown
                .iter()
                .filter(|r| namespace(&r.display_name) == ns)
                .count();
            out.push((ns, count));
        }
        out
    }

    /// Cursor position in whichever popover is open.
    pub fn picker_index(&self) -> usize {
        self.picker.as_ref().map_or(0, Picker::index)
    }

    fn open_namespaces(&mut self) {
        let choices = self.namespace_choices();
        let at = match &self.group {
            None => 0,
            Some(g) => choices.iter().position(|(n, _)| n == g).unwrap_or(0),
        };
        self.picker = Some(Picker::open(choices.len(), at));
        self.pane = Pane::Namespaces;
    }

    /// Apply the picked namespace. Selection returns to the top: the old
    /// index means nothing in a different namespace.
    fn choose_namespace(&mut self) -> Command {
        let choices = self.namespace_choices();
        let index = self.picker_index();
        self.group = match choices.get(index) {
            Some((name, _)) if index > 0 => Some(name.clone()),
            _ => None,
        };
        self.close_picker();
        self.selected = 0;
        self.anchor = None;
        self.clamp_selection();
        self.detail_command()
    }

    fn open_sort(&mut self) {
        let at = drift::Sort::ALL
            .iter()
            .position(|s| *s == self.sort)
            .unwrap_or(0);
        self.picker = Some(Picker::open(drift::Sort::ALL.len(), at));
        self.pane = Pane::Sort;
    }

    fn choose_sort(&mut self) -> Command {
        self.sort = drift::Sort::ALL[self.picker_index()];
        drift::sort_by(&mut self.rows, self.sort);
        self.close_picker();
        self.selected = 0;
        self.anchor = None;
        self.clamp_selection();
        self.detail_command()
    }

    pub fn open_branch_picker(&mut self, repo: PathBuf, rows: Vec<BranchChoice>) {
        let at = rows.iter().position(|b| b.is_head).unwrap_or(0);
        self.picker = Some(Picker::open(rows.len(), at));
        self.branch_choices = rows;
        self.picker_repo = Some(repo);
        self.pane = Pane::Branches;
    }

    pub fn branch_choices(&self) -> &[BranchChoice] {
        &self.branch_choices
    }

    fn choose_branch(&mut self) -> Command {
        let choice = self.branch_choices.get(self.picker_index()).cloned();
        let repo = self.picker_repo.clone();
        self.close_picker();
        match (choice, repo) {
            (Some(c), Some(p)) if !c.is_head => Command::Checkout(p, c.name),
            _ => Command::None,
        }
    }

    /// Leaves the popover open: viewing a diff does not change anything about
    /// the branch list, so there is nothing to re-pick when the pager returns.
    fn diff_highlighted_branch(&self) -> Command {
        let choice = self.branch_choices.get(self.picker_index()).cloned();
        let repo = self.picker_repo.clone();
        match (choice, repo) {
            (Some(c), Some(p)) => Command::BranchDiff(p, c.name),
            _ => Command::None,
        }
    }

    pub fn open_stash_picker(&mut self, repo: PathBuf, rows: Vec<StashChoice>) {
        if rows.is_empty() {
            self.set_toast("no stashes", Duration::from_secs(6));
            return;
        }
        self.picker = Some(Picker::open(rows.len(), 0));
        self.stash_choices = rows;
        self.picker_repo = Some(repo);
        self.pane = Pane::Stashes;
    }

    pub fn stash_choices(&self) -> &[StashChoice] {
        &self.stash_choices
    }

    fn confirm_stash_drop(&mut self) -> Command {
        let (Some(choice), Some(path)) = (
            self.stash_choices.get(self.picker_index()).cloned(),
            self.picker_repo.clone(),
        ) else {
            return Command::None;
        };
        let scope = self.display_name_of(&path);
        self.close_picker();
        self.pending = Some(Pending {
            kind: Destructive::StashDrop,
            targets: vec![path],
            stash: Some(choice.index),
            branch: None,
            summary: format!(
                "drop stash@{{{}}} in {scope} — \"{}\"",
                choice.index, choice.message
            ),
        });
        self.pane = Pane::Confirm;
        Command::None
    }

    /// The current branch is never offered here: it is filtered out of the
    /// picker choice, since git refuses to delete a branch that is checked out.
    fn confirm_branch_delete(&mut self) -> Command {
        let (Some(choice), Some(path)) = (
            self.branch_choices.get(self.picker_index()).cloned(),
            self.picker_repo.clone(),
        ) else {
            return Command::None;
        };
        if choice.is_head {
            self.set_toast("cannot delete the current branch", Duration::from_secs(6));
            return Command::None;
        }
        let scope = self.display_name_of(&path);
        self.close_picker();
        self.pending = Some(Pending {
            kind: Destructive::BranchDelete,
            targets: vec![path],
            stash: None,
            branch: Some(choice.name.clone()),
            summary: format!("delete branch {} in {scope}", choice.name),
        });
        self.pane = Pane::Confirm;
        Command::None
    }

    fn close_picker(&mut self) {
        self.pane = Pane::List;
        self.picker = None;
        self.picker_repo = None;
    }

    /// How a repository is named on screen, by path. Falls back to the path
    /// for a repository the current scan no longer lists.
    fn display_name_of(&self, path: &std::path::Path) -> String {
        self.rows
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.display_name.clone())
            .unwrap_or_else(|| path.display().to_string())
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected(&self) -> Option<&RepoStatus> {
        self.visible().get(self.selected).copied()
    }

    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
    }

    fn move_by(&mut self, delta: isize) -> Command {
        let len = self.visible().len() as isize;
        if len == 0 {
            return Command::None;
        }
        self.selected = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        self.detail_command()
    }

    pub fn pane_scroll(&self) -> u16 {
        self.pane_scroll
    }

    pub fn detail_toggled(&self) -> bool {
        self.detail_toggled
    }

    /// Whether the detail pane/popover should be showing: shown by default
    /// when the name fits (`fits`), hidden when it doesn't — flipped by `Tab`.
    pub fn detail_open(&self, fits: bool) -> bool {
        fits != self.detail_toggled
    }

    /// Wheel over a scrollable pane.
    pub fn scroll_pane_by(&mut self, delta: i16) {
        self.scroll_pane(delta);
    }

    /// Wheel outside the detail pane: the list cursor, unless an overlay has
    /// taken over the screen.
    pub fn scroll_list_by(&mut self, delta: isize) -> Command {
        if matches!(self.pane, Pane::Problems | Pane::Help) {
            self.scroll_pane(delta.clamp(-3, 3) as i16);
            return Command::None;
        }
        self.move_by(delta)
    }

    /// The view knows the rendered height; the state does not, so the bound
    /// arrives after the frame it applies to.
    pub fn clamp_pane_scroll(&mut self, max: u16) {
        self.pane_scroll = self.pane_scroll.min(max);
    }

    fn scroll_pane(&mut self, delta: i16) {
        self.pane_scroll = self.pane_scroll.saturating_add_signed(delta);
    }

    /// A different repository starts at the top of its own detail.
    fn detail_command(&mut self) -> Command {
        self.pane_scroll = 0;
        match self.selected() {
            Some(r) => Command::LoadDetail(r.path.clone()),
            None => Command::None,
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected().map(|r| r.path.clone())
    }

    /// The selected repository, if it has something to compare against.
    fn upstream_target(&mut self) -> Option<PathBuf> {
        let found = self
            .selected()
            .map(|r| (r.path.clone(), r.upstream.is_some()));
        match found {
            Some((path, true)) => Some(path),
            Some((_, false)) => {
                self.set_toast("branch has no upstream", Duration::from_secs(6));
                None
            }
            None => None,
        }
    }

    // --- marks -----------------------------------------------------------

    pub fn is_marked(&self, path: &std::path::Path) -> bool {
        self.marked.contains(path)
    }

    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    pub fn set_default_branches(&mut self, branches: Vec<String>) {
        self.default_branches = branches;
    }

    pub fn default_branches(&self) -> &[String] {
        &self.default_branches
    }

    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    fn toggle_mark(&mut self) {
        if let Some(path) = self.selected_path() {
            let now_marked = !self.marked.remove(&path);
            if now_marked {
                self.marked.insert(path);
            }
            self.anchor = self.selected_path().map(|p| (p, now_marked));
        }
    }

    /// Sweeps the anchor's own outcome (mark or unmark) across the range.
    /// The anchor is a repository, not an index: a background refresh re-sorts
    /// the list, and an index would then point at a different row.
    fn mark_range(&mut self) {
        let (anchor, mark) = match &self.anchor {
            Some((path, mark)) => (
                self.visible()
                    .iter()
                    .position(|r| r.path == *path)
                    .unwrap_or(self.selected),
                *mark,
            ),
            None => (self.selected, true),
        };
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
        self.anchor = self.selected_path().map(|p| (p, mark));
    }

    /// Mark every visible row, or clear the marks if there are any. One key
    /// for both so it is always the way out of a selection.
    fn mark_all(&mut self) {
        if self.marked.is_empty() {
            let paths: Vec<PathBuf> = self.visible().iter().map(|r| r.path.clone()).collect();
            self.marked.extend(paths);
        } else {
            self.marked.clear();
        }
        self.anchor = None;
    }

    /// What an action applies to: every marked repository still visible, or
    /// the selected row when nothing is marked.
    fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            return self.selected_path().into_iter().collect();
        }
        self.visible()
            .iter()
            .filter(|r| self.marked.contains(&r.path))
            .map(|r| r.path.clone())
            .collect()
    }

    /// Act on the marked-or-selected set, saying so when marks exist but the
    /// current view hides every one of them.
    fn act(&mut self, make: fn(Vec<PathBuf>) -> Command) -> Command {
        match non_empty(self.targets()) {
            Some(targets) => make(targets),
            None => {
                self.warn_no_targets();
                Command::None
            }
        }
    }

    fn warn_no_targets(&mut self) {
        if !self.marked.is_empty() {
            self.set_toast(
                format!("{} marked, none of them in this view", self.marked.len()),
                Duration::from_secs(6),
            );
        }
    }

    /// Stage a destructive action for confirmation. Returns `Command::None`:
    /// nothing happens until the user confirms.
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
            stash: None,
            branch: None,
            summary: format!("{} {scope}", kind.verb()),
        });
        self.pane = Pane::Confirm;
        Command::None
    }

    // --- toasts ----------------------------------------------------------

    pub fn set_toast(&mut self, text: impl Into<String>, ttl: Duration) {
        self.toast = Some((text.into(), Instant::now() + ttl));
    }

    pub fn toast(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(t, _)| t.as_str())
    }

    // --- input -----------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) -> Command {
        // Ctrl-C always quits, whatever mode we are in.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Command::Quit;
        }
        // Anything else held down makes this a different key than the one
        // bound: Ctrl-P is not `p`, and Ctrl-W is not a `w` to type into the
        // filter. Shift is excluded — it is how the uppercase keys arrive.
        if key.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META,
        ) {
            return Command::None;
        }
        if self.filtering {
            return self.on_key_filtering(key);
        }
        if self.pane != Pane::List {
            return self.on_key_overlay(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Command::Quit,
            KeyCode::Down => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Char('g') => {
                self.selected = 0;
                self.detail_command()
            }
            KeyCode::Char('G') => {
                self.selected = self.visible().len().saturating_sub(1);
                self.detail_command()
            }
            KeyCode::Char('d') => {
                self.drifted_only = !self.drifted_only;
                self.anchor = None;
                self.clamp_selection();
                self.detail_command()
            }
            KeyCode::Char('/') => {
                self.filtering = true;
                Command::None
            }
            KeyCode::Char('!') => {
                self.pane = Pane::Problems;
                self.pane_scroll = 0;
                Command::None
            }
            KeyCode::Char('?') => {
                self.pane = Pane::Help;
                self.pane_scroll = 0;
                Command::None
            }
            KeyCode::Char('r') => Command::Rescan,
            KeyCode::Tab => {
                self.detail_toggled = !self.detail_toggled;
                Command::None
            }
            KeyCode::Char('F') => Command::FetchAll,
            KeyCode::Char(' ') => {
                self.toggle_mark();
                Command::None
            }
            KeyCode::Char('J') => {
                self.scroll_pane(1);
                Command::None
            }
            KeyCode::Char('K') => {
                self.scroll_pane(-1);
                Command::None
            }
            KeyCode::Char('V') => {
                self.mark_range();
                Command::None
            }
            KeyCode::Char('a') => {
                self.mark_all();
                Command::None
            }
            KeyCode::Char('n') => {
                self.open_namespaces();
                Command::None
            }
            KeyCode::Char('s') => Command::OpenStashes,
            KeyCode::Char('S') => {
                self.open_sort();
                Command::None
            }
            KeyCode::Char('f') => self.act(Command::Fetch),
            KeyCode::Char('p') => {
                let had_marks = !self.marked.is_empty();
                let command = self.act(Command::Pull);
                if had_marks && !matches!(command, Command::None) {
                    self.marked.clear();
                    self.anchor = None;
                }
                command
            }
            // Uppercase only: this destroys work, so a mistyped lowercase key
            // must not reach it. It then waits on a confirmation.
            KeyCode::Char('X') => self.confirm(Destructive::Prune),
            KeyCode::Enter => self.selected_path().map_or(Command::None, Command::Shell),
            KeyCode::Char('e') => self.selected_path().map_or(Command::None, Command::Editor),
            KeyCode::Char('U') => self.upstream_target().map_or(Command::None, Command::Diff),
            KeyCode::Char('L') => self.upstream_target().map_or(Command::None, Command::Log),
            KeyCode::Char('M') => self
                .selected_path()
                .map_or(Command::None, Command::AncestorDiff),
            KeyCode::Char('b') => {
                let dirty = self.selected().map(|r| r.is_dirty());
                match dirty {
                    Some(true) => {
                        self.set_toast("worktree has uncommitted changes", Duration::from_secs(6));
                        Command::None
                    }
                    Some(false) => Command::OpenBranches,
                    None => Command::None,
                }
            }
            _ => Command::None,
        }
    }

    fn on_key_overlay(&mut self, key: KeyEvent) -> Command {
        if self.pane == Pane::Confirm {
            let confirmed = key.code == KeyCode::Char('y');
            let pending = self.pending.take();
            self.pane = Pane::List;
            return match (confirmed, pending) {
                (true, Some(p)) => match p.kind {
                    Destructive::Prune => Command::Prune(p.targets),
                    Destructive::StashDrop => match (p.targets.into_iter().next(), p.stash) {
                        (Some(path), Some(i)) => Command::DropStash(path, i),
                        _ => Command::None,
                    },
                    Destructive::BranchDelete => match (p.targets.into_iter().next(), p.branch) {
                        (Some(path), Some(name)) => Command::DeleteBranch(path, name),
                        _ => Command::None,
                    },
                },
                _ => Command::None,
            };
        }
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
                    self.close_picker();
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
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
                KeyCode::Char('d') => return self.diff_highlighted_branch(),
                KeyCode::Char('x') => return self.confirm_branch_delete(),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('b') => {
                    self.close_picker();
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
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
                    let path = self.picker_repo.clone();
                    self.close_picker();
                    return match (path, index) {
                        (Some(p), Some(i)) => Command::ShowStash(p, i),
                        _ => Command::None,
                    };
                }
                KeyCode::Char('x') => return self.confirm_stash_drop(),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => {
                    self.close_picker();
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
        if self.pane == Pane::Sort {
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
                KeyCode::Enter => return self.choose_sort(),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('S') => {
                    self.close_picker();
                    return Command::None;
                }
                _ => return Command::None,
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.pane = Pane::List;
                self.pane_scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('J') => self.scroll_pane(1),
            KeyCode::Up | KeyCode::Char('K') => self.scroll_pane(-1),
            KeyCode::PageDown => self.scroll_pane(10),
            KeyCode::PageUp => self.scroll_pane(-10),
            KeyCode::Char('!') => {
                self.pane = if self.pane == Pane::Problems {
                    Pane::List
                } else {
                    Pane::Problems
                }
            }
            KeyCode::Char('?') => {
                self.pane = if self.pane == Pane::Help {
                    Pane::List
                } else {
                    Pane::Help
                }
            }
            _ => {}
        }
        Command::None
    }

    fn on_key_filtering(&mut self, key: KeyEvent) -> Command {
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter.clear();
                self.anchor = None;
                self.clamp_selection();
                self.detail_command()
            }
            KeyCode::Enter => {
                self.filtering = false;
                self.detail_command()
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
                self.anchor = None;
                self.clamp_selection();
                self.detail_command()
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.selected = 0;
                self.anchor = None;
                self.clamp_selection();
                self.detail_command()
            }
            _ => Command::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Head;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn row(name: &str, ahead: u32) -> RepoStatus {
        RepoStatus {
            path: PathBuf::from("/tmp").join(name),
            display_name: name.to_string(),
            head: Head::Branch("main".into()),
            upstream: Some("origin/main".into()),
            ahead,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            stash_count: 0,
            local_branches: 0,
            last_commit_time: Some(1),
            fetch_age: None,
            error: None,
        }
    }

    fn row_behind(name: &str) -> RepoStatus {
        RepoStatus {
            behind: 3,
            ..row(name, 0)
        }
    }

    fn row_no_upstream(name: &str) -> RepoStatus {
        RepoStatus {
            upstream: None,
            ..row(name, 0)
        }
    }

    fn app_with(rows: Vec<RepoStatus>) -> App {
        let mut app = App::new();
        for r in rows {
            app.push_status(r);
        }
        app.finish_scan();
        app
    }

    #[test]
    fn rows_arrive_unsorted_and_are_ordered_with_drifted_first() {
        let app = app_with(vec![row("clean", 0), row("drifted", 1)]);
        assert_eq!(app.visible()[0].display_name, "drifted");
    }

    #[test]
    fn selection_moves_and_clamps_at_both_ends() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        assert_eq!(app.selected_index(), 0);
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down)); // past the end
        assert_eq!(app.selected_index(), 2);
        app.on_key(code(KeyCode::Up));
        app.on_key(code(KeyCode::Up));
        app.on_key(code(KeyCode::Up)); // past the start
        assert_eq!(app.selected_index(), 0);
    }

    #[test]
    fn g_and_shift_g_jump_to_the_ends() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(app.selected_index(), 2);
        app.on_key(key('g'));
        assert_eq!(app.selected_index(), 0);
    }

    #[test]
    fn moving_the_selection_requests_the_new_rows_detail() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        let cmd = app.on_key(code(KeyCode::Down));
        let expected = app.selected().unwrap().path.clone();
        assert_eq!(cmd, Command::LoadDetail(expected));
    }

    #[test]
    fn drifted_only_hides_clean_rows_and_keeps_the_selection_valid() {
        let mut app = app_with(vec![row("drifted", 1), row("clean", 0)]);
        app.on_key(code(KeyCode::Down)); // select "clean"
        assert_eq!(app.selected().unwrap().display_name, "clean");
        app.on_key(key('d'));
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.selected().unwrap().display_name, "drifted");
    }

    #[test]
    fn filter_narrows_by_substring_and_escape_clears_it() {
        let mut app = app_with(vec![row("sre/alpha", 1), row("platform/beta", 1)]);
        app.on_key(key('/'));
        app.on_key(key('a'));
        app.on_key(key('l'));
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.visible()[0].display_name, "sre/alpha");
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn backspace_edits_the_filter() {
        let mut app = app_with(vec![row("alpha", 1), row("widget", 1)]);
        app.on_key(key('/'));
        app.on_key(key('a'));
        app.on_key(key('z'));
        assert_eq!(app.visible().len(), 0);
        app.on_key(code(KeyCode::Backspace));
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn keys_are_literal_text_while_filtering() {
        let mut app = app_with(vec![row("quiet", 1)]);
        app.on_key(key('/'));
        app.on_key(key('q'));
        app.on_key(key('d'));
        // q and d are inserted as text, not executed as quit/toggle commands.
        assert_eq!(app.filter_text(), Some("qd"));
        assert!(!app.on_key(key('q')).eq(&Command::Quit));
    }

    #[test]
    fn enter_ends_filtering_without_clearing_the_filter() {
        let mut app = app_with(vec![row("alpha", 1), row("widget", 1)]);
        app.on_key(key('/'));
        app.on_key(key('a'));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.filter_text(), None, "no longer typing");
        assert_eq!(app.visible().len(), 1, "but the filter still applies");
    }

    #[test]
    fn action_keys_emit_commands_for_the_selected_repo() {
        let mut app = app_with(vec![row("a", 1)]);
        let p = app.selected().unwrap().path.clone();
        assert_eq!(app.on_key(key('f')), Command::Fetch(vec![p.clone()]));
        assert_eq!(app.on_key(key('p')), Command::Pull(vec![p.clone()]));
        assert_eq!(app.on_key(code(KeyCode::Enter)), Command::Shell(p.clone()));
        assert_eq!(app.on_key(key('e')), Command::Editor(p));
        assert_eq!(app.on_key(key('s')), Command::OpenStashes);
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            Command::FetchAll
        );
        assert_eq!(app.on_key(key('r')), Command::Rescan);
        assert_eq!(app.on_key(key('q')), Command::Quit);
    }

    #[test]
    fn u_and_l_open_the_pager_for_the_selected_repository() {
        let mut app = app_with(vec![row_behind("a")]);
        let path = app.selected().unwrap().path.clone();
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT)),
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
            app.on_key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT)),
            Command::None
        );
        assert!(app.toast().unwrap().contains("upstream"));
    }

    #[test]
    fn tab_flips_the_detail_toggle_and_flips_back() {
        let mut app = app_with(vec![row("a", 1)]);
        assert!(!app.detail_toggled());
        app.on_key(code(KeyCode::Tab));
        assert!(app.detail_toggled());
        app.on_key(code(KeyCode::Tab));
        assert!(!app.detail_toggled());
    }

    #[test]
    fn detail_open_follows_fit_unless_tab_flips_it() {
        let mut app = app_with(vec![row("a", 1)]);
        assert!(app.detail_open(true), "shown by default when it fits");
        assert!(
            !app.detail_open(false),
            "hidden by default when it does not"
        );

        app.on_key(code(KeyCode::Tab));
        assert!(!app.detail_open(true), "tab hides it despite fitting");
        assert!(app.detail_open(false), "tab shows it despite not fitting");
    }

    #[test]
    fn j_and_k_no_longer_move_the_selection() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        app.on_key(key('j'));
        app.on_key(key('k'));
        assert_eq!(app.selected_index(), 0, "arrows only");
    }

    #[test]
    fn esc_quits_from_the_list() {
        let mut app = app_with(vec![row("a", 1)]);
        assert_eq!(app.on_key(code(KeyCode::Esc)), Command::Quit);
    }

    #[test]
    fn esc_closes_an_overlay_before_it_quits() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key('?'));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Command::None, "closes help");
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(
            app.on_key(code(KeyCode::Esc)),
            Command::Quit,
            "now it quits"
        );
    }

    #[test]
    fn esc_clears_the_filter_before_it_quits() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key('/'));
        app.on_key(key('a'));
        assert_eq!(
            app.on_key(code(KeyCode::Esc)),
            Command::LoadDetail("/tmp/a".into())
        );
        assert_eq!(app.filter_text(), None);
    }

    #[test]
    fn space_marks_and_unmarks_the_selected_row() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        let p = app.selected().unwrap().path.clone();
        app.on_key(key(' '));
        assert!(app.is_marked(&p));
        assert_eq!(app.marked_count(), 1);
        app.on_key(key(' '));
        assert!(!app.is_marked(&p));
        assert_eq!(app.marked_count(), 0);
    }

    #[test]
    fn actions_apply_to_the_marked_set_when_there_is_one() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        app.on_key(key(' ')); // mark row 0
        app.on_key(code(KeyCode::Down));
        app.on_key(key(' ')); // mark row 1
        match app.on_key(key('f')) {
            Command::Fetch(paths) => assert_eq!(paths.len(), 2),
            other => panic!("expected two targets, got {other:?}"),
        }
    }

    #[test]
    fn pull_clears_the_marks_it_acted_on() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        app.on_key(key(' ')); // mark row 0
        app.on_key(code(KeyCode::Down));
        app.on_key(key(' ')); // mark row 1
        match app.on_key(key('p')) {
            Command::Pull(paths) => assert_eq!(paths.len(), 2),
            other => panic!("expected two targets, got {other:?}"),
        }
        assert_eq!(app.marked_count(), 0, "marks should not survive the pull");
    }

    #[test]
    fn actions_fall_back_to_the_current_row_when_nothing_is_marked() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        let p = app.selected().unwrap().path.clone();
        assert_eq!(app.on_key(key('f')), Command::Fetch(vec![p]));
    }

    #[test]
    fn marks_survive_a_rescan_that_still_finds_the_repository() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key(' '));
        assert_eq!(app.marked_count(), 1);
        app.begin_scan();
        app.push_status(row("a", 1));
        app.finish_scan();
        assert_eq!(app.marked_count(), 1, "a refresh is not a reset");
    }

    #[test]
    fn a_mark_is_dropped_when_the_repository_is_gone_after_a_rescan() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key(' '));
        app.begin_scan();
        app.push_status(row("b", 1));
        app.finish_scan();
        assert_eq!(app.marked_count(), 0);
    }

    #[test]
    fn a_held_modifier_makes_it_a_different_key_entirely() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        for c in ['p', 'f', 'r', 'q', 'a', 'V'] {
            let ctrl = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
            assert_eq!(app.on_key(ctrl), Command::None, "Ctrl-{c} must do nothing");
            let alt = KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
            assert_eq!(app.on_key(alt), Command::None, "Alt-{c} must do nothing");
        }
        assert_eq!(app.marked_count(), 0, "no mark slipped through");
        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn ctrl_c_still_quits_despite_the_modifier_guard() {
        let mut app = app_with(vec![row("a", 1)]);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.on_key(ctrl_c), Command::Quit);
    }

    #[test]
    fn a_modified_key_is_not_typed_into_the_filter() {
        let mut app = app_with(vec![row("widget", 1)]);
        app.on_key(key('/'));
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.filter_text(), Some(""), "Ctrl-W is not a w");
    }

    #[test]
    fn only_uppercase_v_marks_a_range() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        app.on_key(key(' '));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down));
        app.on_key(key('v'));
        assert_eq!(app.marked_count(), 1, "lowercase v is not a binding");
        app.on_key(key('V'));
        assert_eq!(app.marked_count(), 3);
    }

    #[test]
    fn only_lowercase_y_confirms_a_destructive_action() {
        for c in ['Y', 'n', 'x', '\n'] {
            let mut app = app_with(vec![row("a", 1)]);
            app.on_key(key('X'));
            assert_eq!(app.pane(), Pane::Confirm);
            assert_eq!(
                app.on_key(key(c)),
                Command::None,
                "the pane says any key but y cancels; {c:?} must cancel"
            );
            assert_eq!(app.pane(), Pane::List);
        }
    }

    #[test]
    fn space_does_not_double_as_choose_in_the_namespace_picker() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(key(' '));
        assert_eq!(app.pane(), Pane::Namespaces, "still open");
        assert_eq!(app.group(), None, "nothing chosen");
        assert_eq!(app.marked_count(), 0, "and nothing marked behind it");
    }

    #[test]
    fn v_marks_the_range_from_the_last_mark_to_the_cursor() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1), row("d", 1)]);
        app.on_key(key(' '));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.marked_count(), 1, "only the anchor is marked so far");
        app.on_key(key('V'));
        assert_eq!(app.marked_count(), 3, "anchor through cursor, inclusive");
    }

    #[test]
    fn a_range_mark_works_upwards_too() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down));
        app.on_key(key(' '));
        app.on_key(code(KeyCode::Up));
        app.on_key(code(KeyCode::Up));
        app.on_key(key('V'));
        assert_eq!(app.marked_count(), 3);
    }

    #[test]
    fn a_range_takes_the_state_the_anchor_landed_in() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1), row("d", 1)]);
        app.on_key(key('a')); // mark all four
        assert_eq!(app.marked_count(), 4);

        app.on_key(code(KeyCode::Down)); // row b
        app.on_key(key(' ')); // unmarks b, anchor = (1, false)
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
    fn a_sweep_follows_its_repository_when_a_refresh_re_sorts_the_list() {
        // A fetch pushes each row's fresh status as it lands, which re-sorts the
        // list mid-selection, so the anchor must not be a row index.
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1), row("d", 1)]);
        app.on_key(key('a'));

        app.on_key(code(KeyCode::Down)); // b
        app.on_key(key(' ')); // unmark b, anchor = b

        // "d" turns out to have diverged and sorts to the top, shifting
        // every other row down one.
        app.push_status(RepoStatus {
            behind: 3,
            ..row("d", 1)
        });
        let names: Vec<&str> = app
            .visible()
            .iter()
            .map(|r| r.display_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["d", "a", "b", "c"],
            "fixture must actually re-sort"
        );

        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down)); // c
        app.on_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));

        let marked: Vec<&str> = app
            .visible()
            .iter()
            .filter(|r| app.is_marked(&r.path))
            .map(|r| r.display_name.as_str())
            .collect();
        assert_eq!(
            marked,
            vec!["d", "a"],
            "only b..c should have been swept clear"
        );
    }

    #[test]
    fn a_range_still_marks_when_the_anchor_was_marked() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        app.on_key(key(' ')); // marks a, anchor = (0, true)
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

    #[test]
    fn a_marks_everything_visible_and_marks_again_to_clear() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        app.on_key(key('a'));
        assert_eq!(app.marked_count(), 3);
        app.on_key(key('a'));
        assert_eq!(app.marked_count(), 0, "pressing it again toggles back off");
    }

    #[test]
    fn a_clears_a_partial_selection_in_one_press() {
        let mut app = app_with(vec![row("a", 1), row("b", 1), row("c", 1)]);
        app.on_key(key(' '));
        assert_eq!(app.marked_count(), 1);
        app.on_key(key('a'));
        assert_eq!(app.marked_count(), 0, "anything marked means a clears");
    }

    #[test]
    fn a_range_mark_does_not_reuse_an_index_from_a_different_filter() {
        let mut app = app_with(vec![
            row("alpha", 1),
            row("beta", 1),
            row("gamma", 1),
            row("widget", 1),
        ]);
        for _ in 0..3 {
            app.on_key(code(KeyCode::Down));
        }
        app.on_key(key(' '));

        app.on_key(key('/'));
        app.on_key(key('a'));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.visible().len(), 3, "widget has no 'a'");

        app.on_key(code(KeyCode::Down));
        app.on_key(key('V'));
        assert_eq!(
            app.marked_count(),
            2,
            "the stale index is dropped; V just marks the row under the cursor"
        );
    }

    #[test]
    fn acting_on_marks_that_the_view_hides_says_so_instead_of_doing_nothing() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(code(KeyCode::Down));
        app.on_key(key(' '));
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.group(), Some("idp"));
        assert_eq!(app.on_key(key('f')), Command::None);
        assert!(
            app.toast().unwrap_or_default().contains("none of them"),
            "{:?}",
            app.toast()
        );
    }

    #[test]
    fn shift_s_opens_a_sort_picker_and_enter_applies_the_choice() {
        let mut app = app_with(vec![row("zzz", 9), row("aaa", 0)]);
        assert_eq!(app.sort_mode(), drift::Sort::Drift);
        assert_eq!(app.visible()[0].display_name, "zzz", "drifted first");

        app.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert_eq!(app.pane(), Pane::Sort);
        assert_eq!(app.picker_index(), 0, "opens on the sort in force");

        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.sort_mode(), drift::Sort::Name);
        assert_eq!(app.visible()[0].display_name, "aaa");
    }

    #[test]
    fn esc_leaves_the_sort_picker_without_changing_anything() {
        let mut app = app_with(vec![row("zzz", 9), row("aaa", 0)]);
        app.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.sort_mode(), drift::Sort::Drift, "unchanged");
    }

    #[test]
    fn n_opens_a_picker_listing_every_namespace_plus_all() {
        let mut app = app_with(vec![
            row("idp/one", 1),
            row("idp/two", 1),
            row("mkp/three", 1),
        ]);
        app.on_key(key('n'));
        assert_eq!(app.pane(), Pane::Namespaces);
        assert_eq!(
            app.namespace_choices(),
            vec![
                ("all".to_string(), 3),
                ("idp".to_string(), 2),
                ("mkp".to_string(), 1),
            ]
        );
    }

    #[test]
    fn picking_a_namespace_filters_the_list_to_it() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.group(), Some("idp"));
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn picking_all_clears_the_namespace_filter() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Enter));
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Up));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.group(), None);
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn the_picker_opens_on_the_namespace_already_in_force() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.group(), Some("mkp"));
        app.on_key(key('n'));
        assert_eq!(app.picker_index(), 2);
    }

    #[test]
    fn escaping_the_picker_leaves_the_filter_alone() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.group(), None);
    }

    #[test]
    fn actions_only_target_repositories_the_namespace_filter_still_shows() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(key('a'));
        assert_eq!(app.marked_count(), 2);
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Enter));
        match app.on_key(key('f')) {
            Command::Fetch(paths) => assert_eq!(paths.len(), 1, "{paths:?}"),
            other => panic!("expected a fetch, got {other:?}"),
        }
    }

    #[test]
    fn pruning_asks_for_confirmation_before_deleting_branches() {
        let mut app = app_with(vec![row("a", 1)]);
        assert_eq!(app.on_key(key('X')), Command::None);
        assert_eq!(app.pane(), Pane::Confirm);
        assert!(app.pending().unwrap().summary.contains("upstream is gone"));
        match app.on_key(key('y')) {
            Command::Prune(paths) => assert_eq!(paths.len(), 1),
            other => panic!("expected a prune, got {other:?}"),
        }
    }

    #[test]
    fn any_other_key_cancels_a_destructive_action() {
        for cancel in ['n', 'q', 'X'] {
            let mut app = app_with(vec![row("a", 1)]);
            app.on_key(key('X'));
            assert_eq!(
                app.on_key(key(cancel)),
                Command::None,
                "{cancel} must cancel"
            );
            assert_eq!(app.pane(), Pane::List);
            assert!(app.pending().is_none(), "the pending action is dropped");
        }
    }

    #[test]
    fn escape_cancels_a_destructive_action_rather_than_quitting() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key('X'));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Command::None);
        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn destructive_keys_are_inert_with_no_selection() {
        let mut app = App::new();
        app.finish_scan();
        assert_eq!(app.on_key(key('X')), Command::None);
        assert_eq!(app.pane(), Pane::List, "no confirmation for nothing");
    }

    #[test]
    fn the_removed_destructive_keys_do_nothing() {
        let mut app = app_with(vec![row("a", 1)]);
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT)),
            Command::None,
            "C should no longer be bound"
        );
        assert_eq!(app.pane(), Pane::List, "C should not open a confirmation");
    }

    #[test]
    fn a_non_default_branch_is_styled_differently_from_a_default_one() {
        use crate::theme::branch_style;
        let defaults: Vec<String> = ["main", "master"].iter().map(|s| s.to_string()).collect();
        let mut ordinary = row("a", 0);
        ordinary.head = Head::Branch("main".into());
        let mut feature = row("b", 0);
        feature.head = Head::Branch("feat/thing".into());
        assert_ne!(
            branch_style(&ordinary, &defaults),
            branch_style(&feature, &defaults),
            "a non-default branch must stand out"
        );
    }

    #[test]
    fn ctrl_c_quits_even_while_filtering() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key('/'));
        let cmd = app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(cmd, Command::Quit);
    }

    #[test]
    fn action_keys_are_inert_when_there_is_no_selection() {
        let mut app = App::new();
        app.finish_scan();
        assert_eq!(app.on_key(key('f')), Command::None);
        assert_eq!(app.on_key(code(KeyCode::Enter)), Command::None);
    }

    #[test]
    fn panes_toggle_and_escape_returns_to_the_list() {
        let mut app = app_with(vec![row("a", 1)]);
        app.push_problem(Problem {
            path: PathBuf::from("/x"),
            reason: "denied".into(),
        });
        app.on_key(key('!'));
        assert_eq!(app.pane(), Pane::Problems);
        app.on_key(key('!'));
        assert_eq!(app.pane(), Pane::List);
        app.on_key(key('?'));
        assert_eq!(app.pane(), Pane::Help);
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn a_toast_expires() {
        let mut app = App::new();
        app.set_toast("hello", Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(app.toast(), None);
    }

    #[test]
    fn a_status_arriving_twice_for_one_repo_replaces_it_rather_than_duplicating() {
        let mut app = App::new();
        app.push_status(row("a", 0));
        app.push_status(row("a", 3));
        app.finish_scan();
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.visible()[0].ahead, 3);
    }

    #[test]
    fn begin_scan_clears_previous_results() {
        let mut app = app_with(vec![row("a", 1)]);
        app.begin_scan();
        assert!(app.visible().is_empty());
        assert!(app.is_scanning());
    }

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
        assert_eq!(
            (scan.done, scan.total),
            (1, 0),
            "a scan does not know its size"
        );
        app.finish_scan();
        assert!(!app.is_scanning());
        assert!(app.jobs().is_empty());
    }

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

    fn branch(name: &str, is_head: bool) -> BranchChoice {
        BranchChoice {
            name: name.into(),
            ahead: 0,
            behind: 0,
            has_upstream: true,
            is_head,
        }
    }

    #[test]
    fn b_refuses_to_switch_with_uncommitted_work() {
        let mut app = app_with(vec![RepoStatus {
            unstaged: 2,
            ..row("a", 0)
        }]);
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
        app.open_branch_picker(
            path.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
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
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(path, vec![branch("main", true)]);
        assert_eq!(app.on_key(code(KeyCode::Enter)), Command::None);
        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn esc_leaves_the_branch_picker_without_switching() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(path, vec![branch("main", true), branch("other", false)]);
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Command::None);
        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn d_diffs_the_highlighted_branch_without_checking_it_out() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(
            path.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
        app.on_key(code(KeyCode::Down));
        assert_eq!(
            app.on_key(key('d')),
            Command::BranchDiff(path, "feat/retries".to_string())
        );
        assert_eq!(
            app.pane(),
            Pane::Branches,
            "stays open so the pager can return to it"
        );
    }

    #[test]
    fn the_popover_still_works_normally_after_diffing_from_it() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(
            path.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
        app.on_key(code(KeyCode::Down));
        app.on_key(key('d'));
        assert_eq!(app.picker_index(), 1, "the cursor stayed put");
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            Command::Checkout(path, "feat/retries".to_string()),
            "the same branch is still selected"
        );
    }

    #[test]
    fn d_also_works_on_the_currently_checked_out_branch() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(path.clone(), vec![branch("main", true)]);
        assert_eq!(
            app.on_key(key('d')),
            Command::BranchDiff(path, "main".to_string())
        );
    }

    #[test]
    fn a_branch_diff_targets_the_repository_the_popover_listed() {
        let mut app = app_with(vec![row("a", 0), row("b", 0)]);
        let repo_a = app.visible()[0].path.clone();
        app.open_branch_picker(
            repo_a.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
        slide_cursor_off(&mut app, &repo_a);
        app.on_key(code(KeyCode::Down));
        assert_eq!(
            app.on_key(key('d')),
            Command::BranchDiff(repo_a, "feat/retries".to_string())
        );
    }

    #[test]
    fn deleting_a_branch_asks_first() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(
            path.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.on_key(key('x')), Command::None);
        assert_eq!(app.pane(), Pane::Confirm);
        let pending = app.pending().unwrap();
        assert_eq!(pending.branch, Some("feat/retries".to_string()));
        assert_eq!(
            app.on_key(key('y')),
            Command::DeleteBranch(path, "feat/retries".to_string())
        );
    }

    #[test]
    fn the_current_branch_cannot_be_deleted_from_the_picker() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_branch_picker(path, vec![branch("main", true)]);
        assert_eq!(app.on_key(key('x')), Command::None);
        assert_eq!(app.pane(), Pane::Branches, "still open, nothing to confirm");
        assert!(app.toast().unwrap().contains("current branch"));
    }

    #[test]
    fn a_branch_delete_targets_the_repository_the_popover_listed() {
        let mut app = app_with(vec![row("a", 0), row("b", 0)]);
        let repo_a = app.visible()[0].path.clone();
        app.open_branch_picker(
            repo_a.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
        slide_cursor_off(&mut app, &repo_a);
        app.on_key(code(KeyCode::Down));
        app.on_key(key('x'));
        assert_eq!(
            app.on_key(key('y')),
            Command::DeleteBranch(repo_a, "feat/retries".to_string())
        );
    }

    #[test]
    fn shift_m_diffs_the_selected_repository_against_its_ancestor() {
        let mut app = app_with(vec![row_no_upstream("a")]);
        let path = app.selected().unwrap().path.clone();
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT)),
            Command::AncestorDiff(path),
            "no upstream is required, unlike U"
        );
    }

    fn stash(index: usize, message: &str) -> StashChoice {
        StashChoice {
            index,
            message: message.into(),
        }
    }

    #[test]
    fn enter_views_a_stash_in_the_pager() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_stash_picker(path.clone(), vec![stash(0, "wip"), stash(1, "other")]);
        app.on_key(code(KeyCode::Down));
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            Command::ShowStash(path, 1)
        );
    }

    #[test]
    fn dropping_a_stash_asks_first() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_stash_picker(path, vec![stash(0, "wip: retry backoff")]);
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
        app.open_stash_picker(path.clone(), vec![stash(0, "wip")]);
        app.on_key(key('x'));
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
            Command::None,
            "the pane promises any other key cancels"
        );

        app.open_stash_picker(path.clone(), vec![stash(0, "wip")]);
        app.on_key(key('x'));
        assert_eq!(app.on_key(key('y')), Command::DropStash(path, 0));
    }

    #[test]
    fn s_asks_the_loop_for_the_stash_list() {
        let mut app = app_with(vec![row("a", 0)]);
        assert_eq!(app.on_key(key('s')), Command::OpenStashes);
    }

    #[test]
    fn an_empty_stash_list_opens_nothing() {
        let mut app = app_with(vec![row("a", 0)]);
        app.open_stash_picker(PathBuf::from("/tmp/a"), Vec::new());
        assert_eq!(app.pane(), Pane::List);
        assert!(app.toast().unwrap().contains("no stashes"));
    }

    #[test]
    fn x_does_nothing_in_the_list_pane() {
        let mut app = app_with(vec![row("a", 0)]);
        assert_eq!(app.on_key(key('x')), Command::None);
        assert_eq!(app.pane(), Pane::List);
    }

    /// A refresh landing while a popover is up re-sorts the list under it, so
    /// the row at the cursor is no longer the repository the popover lists.
    fn slide_cursor_off(app: &mut App, away_from: &std::path::Path) {
        app.push_status(row("b", 9));
        assert_ne!(
            app.selected().unwrap().path,
            away_from,
            "the cursor must now be on a different repository"
        );
    }

    #[test]
    fn a_stash_drop_targets_the_repository_the_popover_listed() {
        let mut app = app_with(vec![row("a", 0), row("b", 0)]);
        let repo_a = app.visible()[0].path.clone();
        app.open_stash_picker(repo_a.clone(), vec![stash(0, "wip: retry backoff")]);
        slide_cursor_off(&mut app, &repo_a);
        app.on_key(key('x'));
        assert_eq!(app.pending().unwrap().targets, vec![repo_a.clone()]);
        assert_eq!(app.on_key(key('y')), Command::DropStash(repo_a, 0));
    }

    #[test]
    fn viewing_a_stash_targets_the_repository_the_popover_listed() {
        let mut app = app_with(vec![row("a", 0), row("b", 0)]);
        let repo_a = app.visible()[0].path.clone();
        app.open_stash_picker(repo_a.clone(), vec![stash(0, "wip")]);
        slide_cursor_off(&mut app, &repo_a);
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            Command::ShowStash(repo_a, 0)
        );
    }

    #[test]
    fn a_checkout_targets_the_repository_the_popover_listed() {
        let mut app = app_with(vec![row("a", 0), row("b", 0)]);
        let repo_a = app.visible()[0].path.clone();
        app.open_branch_picker(
            repo_a.clone(),
            vec![branch("main", true), branch("feat/retries", false)],
        );
        slide_cursor_off(&mut app, &repo_a);
        app.on_key(code(KeyCode::Down));
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            Command::Checkout(repo_a, "feat/retries".into())
        );
    }

    #[test]
    fn the_stash_confirmation_names_the_repository() {
        let mut app = app_with(vec![row("idp/api", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_stash_picker(path, vec![stash(2, "wip: retry backoff")]);
        app.on_key(key('x'));
        assert_eq!(
            app.pending().unwrap().summary,
            "drop stash@{2} in idp/api — \"wip: retry backoff\""
        );
    }

    #[test]
    fn a_rescan_closes_an_open_popover() {
        let mut app = app_with(vec![row("a", 0)]);
        let path = app.selected().unwrap().path.clone();
        app.open_stash_picker(path, vec![stash(0, "wip")]);
        assert_eq!(app.pane(), Pane::Stashes);
        app.begin_scan();
        assert_eq!(app.pane(), Pane::List, "the list it described is gone");
    }
}
