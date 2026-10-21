use std::io::{self, IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;

use crate::actions;
use crate::config::Config;
use crate::detail::{self, RepoDetail};
use crate::drift::is_drifted;
use crate::scan::{self, Event as ScanEvent, ScanOptions};
use crate::status::{self, RepoStatus};
use crate::ui::state::{App, BranchChoice, Command, Pane, StashChoice};
use crate::ui::view;

const TOAST_TTL: Duration = Duration::from_secs(6);
const TICK: Duration = Duration::from_millis(80);

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Messages produced by background work.
enum Msg {
    Scan(ScanEvent, u64),
    Detail(PathBuf, u64, Box<RepoDetail>),
    ActionDone(String),
    /// A background action re-inspected a repo; carries the fresh status and a toast.
    Refresh(RepoStatus, String),
    JobStep(u64),
    JobEnd(u64),
}

fn enter() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    let setup = (|| -> anyhow::Result<Term> {
        io::stdout().execute(EnterAlternateScreen)?;
        io::stdout().execute(EnableMouseCapture)?;
        Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
    })();
    if setup.is_err() {
        // Raw mode is already on; don't strand the user's terminal.
        let _ = disable_raw_mode();
    }
    setup
}

fn leave(term: &mut Term) -> anyhow::Result<()> {
    disable_raw_mode()?;
    let _ = term.backend_mut().execute(DisableMouseCapture);
    term.backend_mut().execute(LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

/// Drop out of the TUI, run something interactive, come back.
fn suspend<T>(term: &mut Term, f: impl FnOnce() -> T) -> anyhow::Result<T> {
    leave(term)?;
    let out = f();
    *term = enter()?;
    term.clear()?;
    Ok(out)
}

pub fn run(opts: ScanOptions, cfg: &Config, drifted_only: bool) -> anyhow::Result<()> {
    if !io::stdout().is_terminal() {
        anyhow::bail!("not a terminal; use --plain or --json");
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let mut app = App::new();
    app.set_default_branches(cfg.default_branches.clone());
    if drifted_only {
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    spawn_scan(opts.clone(), 0, tx.clone());

    // Never leave the user's terminal in raw mode, even on a panic during
    // setup, teardown, or the loop itself.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));

    let mut term = enter()?;

    let mut loop_state = LoopState {
        current_detail: None,
        detail_pending: None,
        detail_area: None,
        generation: 0,
        list_state: ListState::default(),
        incoming_commits: cfg.incoming_commits,
    };

    let result = event_loop(&mut term, &mut app, cfg, &opts, &tx, &rx, &mut loop_state);

    leave(&mut term)?;
    result?;
    Ok(())
}

/// The `&mut` locals threaded through `event_loop` across iterations,
/// gathered so the loop itself keeps a manageable parameter count.
struct LoopState {
    current_detail: Option<(PathBuf, RepoDetail)>,
    /// Set while a `spawn_detail` for it is in flight, so a status update
    /// that lands before the result does doesn't fire a second request.
    detail_pending: Option<PathBuf>,
    /// Where the detail pane was last drawn, for routing the mouse wheel.
    detail_area: Option<ratatui::layout::Rect>,
    generation: u64,
    list_state: ListState,
    incoming_commits: usize,
}

fn event_loop(
    term: &mut Term,
    app: &mut App,
    cfg: &Config,
    opts: &ScanOptions,
    tx: &mpsc::Sender<Msg>,
    rx: &mpsc::Receiver<Msg>,
    ls: &mut LoopState,
) -> anyhow::Result<()> {
    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                // Drop events from a scan generation superseded by a later rescan.
                Msg::Scan(_, gen) if gen != ls.generation => {}
                Msg::Scan(ScanEvent::Status(s), _) => app.push_status(*s),
                Msg::Scan(ScanEvent::Problem(p), _) => app.push_problem(p),
                Msg::Scan(ScanEvent::Done, _) => {
                    app.finish_scan();
                    ensure_detail_for_selection(app, ls, tx);
                }
                Msg::Detail(path, gen, d) => {
                    if ls.detail_pending.as_deref() == Some(path.as_path()) {
                        ls.detail_pending = None;
                    }
                    // Reject results for a generation or a selection the user has since moved past.
                    if gen == ls.generation && app.selected().is_some_and(|s| s.path == path) {
                        ls.current_detail = Some((path, *d));
                    }
                }
                Msg::ActionDone(text) => app.set_toast(text, TOAST_TTL),
                Msg::Refresh(status, text) => {
                    let path = status.path.clone();
                    app.push_status(status);
                    if !text.is_empty() {
                        app.set_toast(text, TOAST_TTL);
                    }
                    sync_detail_after_status_change(app, ls, tx, path);
                }
                Msg::JobStep(id) => app.advance_job(id),
                Msg::JobEnd(id) => {
                    app.end_job(id);
                    if app.order_stale() {
                        if let Some(done) = app
                            .toast()
                            .filter(|t| !t.contains("order changed"))
                            .map(str::to_string)
                        {
                            app.set_toast(
                                format!("{done}  ·  order changed, o to re-sort"),
                                TOAST_TTL,
                            );
                        }
                    }
                }
            }
        }

        // Only show detail that belongs to the currently selected row.
        let detail_for_selection = match (app.selected(), &ls.current_detail) {
            (Some(sel), Some((path, d))) if *path == sel.path => Some(d),
            _ => None,
        };
        let mut rendered = view::Rendered::default();
        term.draw(|f| rendered = view::draw(f, app, detail_for_selection, &mut ls.list_state))?;
        ls.detail_area = rendered.detail_area;
        app.clamp_pane_scroll(rendered.pane_max_scroll);

        if !event::poll(TICK)? {
            continue;
        }
        let command = match event::read()? {
            // With no detail pane on screen, paging has only the list to move.
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && key.modifiers.is_empty()
                    && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
                    && ls.detail_area.is_none()
                    && app.pane() == Pane::List
                    && app.filter_text().is_none() =>
            {
                app.scroll_list_by(if key.code == KeyCode::PageDown {
                    10
                } else {
                    -10
                })
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
            Event::Mouse(m) => {
                let delta = match m.kind {
                    MouseEventKind::ScrollDown => 1,
                    MouseEventKind::ScrollUp => -1,
                    _ => continue,
                };
                // The wheel acts on whatever it is over; with no detail pane on
                // screen there is only the list to scroll.
                match ls.detail_area {
                    Some(a) if over(a, m.column, m.row) => {
                        app.scroll_pane_by(3 * delta as i16);
                        Command::None
                    }
                    _ => app.scroll_list_by(delta as isize),
                }
            }
            _ => continue,
        };

        match command {
            Command::None => {}
            Command::Quit => return Ok(()),
            Command::LoadDetail(path) => request_detail(ls, tx, path),
            Command::Rescan => do_rescan(app, ls, opts, tx, true),
            Command::Fetch(paths) => {
                let targets = named(app, paths);
                let job = app.begin_job("fetching", targets.len());
                spawn_fetch(targets, cfg.concurrency, job, tx.clone());
            }
            Command::FetchAll => {
                let paths: Vec<PathBuf> = app.visible().iter().map(|r| r.path.clone()).collect();
                let targets = named(app, paths);
                let job = app.begin_job("fetching", targets.len());
                spawn_fetch(targets, cfg.concurrency, job, tx.clone());
            }
            Command::Pull(paths) => {
                // Refuse the un-pullable up front, so the user gets the specific
                // reason instantly rather than after a thread hop.
                let mut ready = Vec::new();
                let mut refused = Vec::new();
                for path in paths {
                    if let Some(sel) = status_for(app, &path) {
                        match actions::can_pull(&sel) {
                            Ok(()) => ready.push(sel),
                            Err(e) => refused.push(format!("{}: {e}", sel.display_name)),
                        }
                    }
                }
                if !refused.is_empty() {
                    app.set_toast(refused.join("  ·  "), TOAST_TTL);
                } else if !ready.is_empty() {
                    app.set_toast(format!("pulling {} repositories…", ready.len()), TOAST_TTL);
                }
                for sel in ready {
                    spawn_pull(sel.path.clone(), sel, tx.clone());
                }
            }
            Command::PullAll => {
                let mut ready = Vec::new();
                let mut skipped = 0;
                for row in app.visible().iter().filter(|r| is_drifted(r)) {
                    match actions::can_pull(row) {
                        Ok(()) => ready.push((*row).clone()),
                        Err(_) => skipped += 1,
                    }
                }
                let mut msg = format!("pulling {} repositories…", ready.len());
                if skipped > 0 {
                    msg.push_str(&format!("  ·  {skipped} skipped (dirty or no upstream)"));
                }
                app.set_toast(msg, TOAST_TTL);
                for sel in ready {
                    spawn_pull(sel.path.clone(), sel, tx.clone());
                }
            }
            Command::Prune(paths) => {
                let targets = named(app, paths);
                let job = app.begin_job("pruning", targets.len());
                spawn_concurrent_worktree_op(
                    targets,
                    actions::prune_gone_branches,
                    cfg.concurrency,
                    "pruned",
                    job,
                    tx.clone(),
                );
            }
            Command::Shell(path) => {
                let name = display_name_of(app, &path);
                if let Err(e) = suspend(term, || actions::open_shell(&path))? {
                    app.set_toast(e.to_string(), TOAST_TTL);
                }
                refresh_one(app, &path, &name);
                sync_detail_after_status_change(app, ls, tx, path);
            }
            Command::Lazygit(_) if !actions::lazygit_on_path() => {
                app.set_toast("lazygit not found on PATH", TOAST_TTL);
            }
            Command::Lazygit(path) => {
                let name = display_name_of(app, &path);
                if let Err(e) = suspend(term, || actions::open_lazygit(&path))? {
                    app.set_toast(e.to_string(), TOAST_TTL);
                }
                refresh_one(app, &path, &name);
                sync_detail_after_status_change(app, ls, tx, path);
            }
            Command::OpenWeb(path, branch) => {
                match actions::open_on_web(&path, branch.as_deref()) {
                    Ok(url) => app.set_toast(format!("opening {url}"), TOAST_TTL),
                    Err(e) => app.set_toast(e.to_string(), TOAST_TTL),
                }
            }
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
            Command::AncestorDiff(path) => match ls.current_detail.as_ref() {
                Some((p, d)) if *p == path => match default_branch_of(cfg, d) {
                    Some(base) => {
                        if let Err(e) = suspend(term, || actions::view_ancestor_diff(&path, &base))?
                        {
                            app.set_toast(e.to_string(), TOAST_TTL);
                        }
                    }
                    None => app.set_toast(no_default_branch_toast(cfg), TOAST_TTL),
                },
                _ => app.set_toast("still loading…", TOAST_TTL),
            },
            Command::BranchDiff(path, branch) => match ls.current_detail.as_ref() {
                Some((p, d)) if *p == path => match default_branch_of(cfg, d) {
                    Some(base) => {
                        if let Err(e) =
                            suspend(term, || actions::view_branch_diff(&path, &base, &branch))?
                        {
                            app.set_toast(e.to_string(), TOAST_TTL);
                        }
                    }
                    None => app.set_toast(no_default_branch_toast(cfg), TOAST_TTL),
                },
                _ => app.set_toast("still loading…", TOAST_TTL),
            },
            // Detail is only ever shown, or acted on, for the row it belongs
            // to: it outlives a cursor move until the next Msg::Detail lands.
            Command::OpenBranches => match (app.selected(), ls.current_detail.as_ref()) {
                (Some(sel), Some((path, d))) if *path == sel.path => {
                    let repo = path.clone();
                    let rows = d
                        .branches
                        .iter()
                        .map(|b| BranchChoice {
                            name: b.name.clone(),
                            ahead: b.ahead,
                            behind: b.behind,
                            has_upstream: b.upstream.is_some(),
                            is_head: b.is_head,
                        })
                        .collect();
                    app.open_branch_picker(repo, rows);
                }
                _ => app.set_toast("still loading…", TOAST_TTL),
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
            Command::OpenStashes => match (app.selected(), ls.current_detail.as_ref()) {
                (Some(sel), Some((path, d))) if *path == sel.path => {
                    let repo = path.clone();
                    let rows = d
                        .stashes
                        .iter()
                        .map(|s| StashChoice {
                            index: s.index,
                            message: s.message.clone(),
                        })
                        .collect();
                    app.open_stash_picker(repo, rows);
                }
                _ => app.set_toast("still loading…", TOAST_TTL),
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
            Command::DeleteBranch(path, branch) => {
                let name = display_name_of(app, &path);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let msg = match actions::delete_branch(&path, &branch) {
                        Ok(out) => Msg::Refresh(status::inspect(&path, name), out),
                        Err(e) => Msg::ActionDone(format!("{name}: {e}")),
                    };
                    let _ = tx.send(msg);
                });
            }
        }
    }
}

/// `sort_after` is for a rescan the user asked for; any other caller passes
/// `false` so the list does not move under the cursor.
fn do_rescan(
    app: &mut App,
    ls: &mut LoopState,
    opts: &ScanOptions,
    tx: &mpsc::Sender<Msg>,
    sort_after: bool,
) {
    app.begin_scan(sort_after);
    ls.current_detail = None;
    ls.detail_pending = None;
    ls.generation += 1;
    spawn_scan(opts.clone(), ls.generation, tx.clone());
    ensure_detail_for_selection(app, ls, tx);
}

fn over(a: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= a.x && col < a.x + a.width && row >= a.y && row < a.y + a.height
}

/// Request detail for `path`, tracking it as in flight.
fn request_detail(ls: &mut LoopState, tx: &mpsc::Sender<Msg>, path: PathBuf) {
    ls.detail_pending = Some(path.clone());
    spawn_detail(path, ls.generation, ls.incoming_commits, tx.clone());
}

/// Request detail for whatever is currently selected, unless it's already
/// loaded or already in flight — e.g. after a rescan repopulates the list, so
/// the pane doesn't sit on "loading…" until a keypress.
fn ensure_detail_for_selection(app: &App, ls: &mut LoopState, tx: &mpsc::Sender<Msg>) {
    let Some(path) = app.selected().map(|s| s.path.clone()) else {
        return;
    };
    let loaded = ls.current_detail.as_ref().is_some_and(|(p, _)| *p == path);
    let pending = ls.detail_pending.as_deref() == Some(path.as_path());
    if !loaded && !pending {
        request_detail(ls, tx, path);
    }
}

/// Reloads whatever is now under the cursor after a status update re-sorts
/// the list — the same repo (stale detail) or a different one.
fn sync_detail_after_status_change(
    app: &App,
    ls: &mut LoopState,
    tx: &mpsc::Sender<Msg>,
    changed: PathBuf,
) {
    if app.selected().is_some_and(|s| s.path == changed) {
        request_detail(ls, tx, changed);
    } else {
        ensure_detail_for_selection(app, ls, tx);
    }
}

fn status_for(app: &App, path: &Path) -> Option<RepoStatus> {
    app.visible()
        .iter()
        .find(|r| r.path == path)
        .map(|r| (*r).clone())
}

/// Never more than `concurrency` at a time. Re-inspects each repository so
/// the list reflects the result.
fn spawn_concurrent_worktree_op(
    targets: Vec<(PathBuf, String)>,
    op: fn(&Path) -> Result<String, actions::ActionError>,
    concurrency: usize,
    past_tense: &'static str,
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
                        match op(&path) {
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
            format!("{past_tense} {total} repositories")
        } else {
            format!("{past_tense} {}/{total} ({failed} failed)", total - failed)
        };
        let _ = tx.send(Msg::ActionDone(text));
        let _ = tx.send(Msg::JobEnd(job));
    });
}

/// Pair each path with the name the list shows it under. The worker
/// re-inspects the repository, and `inspect` cannot recover that name itself.
fn named(app: &App, paths: Vec<PathBuf>) -> Vec<(PathBuf, String)> {
    paths
        .into_iter()
        .map(|p| {
            let name = display_name_of(app, &p);
            (p, name)
        })
        .collect()
}

/// The first of the configured default branches that actually exists locally.
fn default_branch_of(cfg: &Config, detail: &RepoDetail) -> Option<String> {
    cfg.default_branches
        .iter()
        .find(|name| detail.branches.iter().any(|b| &b.name == *name))
        .cloned()
}

fn no_default_branch_toast(cfg: &Config) -> String {
    format!(
        "no default branch found locally ({})",
        cfg.default_branches.join("/")
    )
}

fn display_name_of(app: &App, path: &Path) -> String {
    app.visible()
        .iter()
        .find(|r| r.path == path)
        .map(|r| r.display_name.clone())
        .unwrap_or_else(|| path.display().to_string())
}

fn refresh_one(app: &mut App, path: &Path, display_name: &str) {
    app.push_status(status::inspect(path, display_name.to_string()));
}

fn spawn_scan(opts: ScanOptions, generation: u64, tx: mpsc::Sender<Msg>) {
    std::thread::spawn(move || {
        for ev in scan::stream(opts) {
            let done = matches!(ev, ScanEvent::Done);
            if tx.send(Msg::Scan(ev, generation)).is_err() || done {
                break;
            }
        }
    });
}

fn spawn_pull(path: PathBuf, sel: RepoStatus, tx: mpsc::Sender<Msg>) {
    std::thread::spawn(move || {
        let msg = match actions::pull(&path, &sel) {
            Ok(_) => {
                let status = status::inspect(&path, sel.display_name.clone());
                Msg::Refresh(status, format!("pulled {}", sel.display_name))
            }
            Err(e) => Msg::ActionDone(format!("{}: {e}", sel.display_name)),
        };
        let _ = tx.send(msg);
    });
}

fn spawn_detail(path: PathBuf, generation: u64, incoming_limit: usize, tx: mpsc::Sender<Msg>) {
    std::thread::spawn(move || {
        let d = detail::inspect(&path, incoming_limit);
        let _ = tx.send(Msg::Detail(path, generation, Box::new(d)));
    });
}

fn spawn_fetch(
    targets: Vec<(PathBuf, String)>,
    concurrency: usize,
    job: u64,
    tx: mpsc::Sender<Msg>,
) {
    spawn_concurrent_worktree_op(targets, actions::fetch, concurrency, "fetched", job, tx);
}
