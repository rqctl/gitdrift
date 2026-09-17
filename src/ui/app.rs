use std::io::{self, IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
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
use crate::scan::{self, Event as ScanEvent, ScanOptions};
use crate::status::{self, RepoStatus};
use crate::ui::state::{App, BranchChoice, Command, StashChoice};
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
        detail_area: None,
        generation: 0,
        list_state: ListState::default(),
        rescan_after: None,
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
    /// Where the detail pane was last drawn, for routing the mouse wheel.
    detail_area: Option<ratatui::layout::Rect>,
    generation: u64,
    list_state: ListState,
    /// The job whose completion owes a full rescan, set by fetch-all.
    rescan_after: Option<u64>,
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
                    request_current_detail(app, ls.generation, tx);
                }
                Msg::Detail(path, gen, d) => {
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
                    if app.selected().is_some_and(|s| s.path == path) {
                        spawn_detail(path, ls.generation, tx.clone());
                    }
                }
                Msg::JobStep(id) => app.advance_job(id),
                Msg::JobEnd(id) => {
                    app.end_job(id);
                    if ls.rescan_after == Some(id) {
                        ls.rescan_after = None;
                        do_rescan(app, ls, opts, tx);
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
            Command::LoadDetail(path) => spawn_detail(path, ls.generation, tx.clone()),
            Command::Rescan => do_rescan(app, ls, opts, tx),
            Command::Fetch(paths) => {
                let targets = named(app, paths);
                let job = app.begin_job("fetching", targets.len());
                spawn_fetch(targets, cfg.concurrency, job, tx.clone());
            }
            Command::FetchAll => {
                let paths: Vec<PathBuf> = app.visible().iter().map(|r| r.path.clone()).collect();
                let targets = named(app, paths);
                let job = app.begin_job("fetching", targets.len());
                ls.rescan_after = Some(job);
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
            }
            Command::Editor(path) => {
                let name = display_name_of(app, &path);
                let editor = actions::resolve_editor(cfg.editor.as_deref());
                if let Err(e) = suspend(term, || actions::open_editor(&path, &editor))? {
                    app.set_toast(e.to_string(), TOAST_TTL);
                }
                refresh_one(app, &path, &name);
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

fn do_rescan(app: &mut App, ls: &mut LoopState, opts: &ScanOptions, tx: &mpsc::Sender<Msg>) {
    app.begin_scan();
    ls.current_detail = None;
    ls.generation += 1;
    spawn_scan(opts.clone(), ls.generation, tx.clone());
    request_current_detail(app, ls.generation, tx);
}

fn over(a: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= a.x && col < a.x + a.width && row >= a.y && row < a.y + a.height
}

/// Re-request detail for whatever is currently selected, e.g. after a rescan
/// repopulates the list, so the pane doesn't sit on "loading…" until a keypress.
fn request_current_detail(app: &App, generation: u64, tx: &mpsc::Sender<Msg>) {
    if let Some(sel) = app.selected() {
        spawn_detail(sel.path.clone(), generation, tx.clone());
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

fn spawn_detail(path: PathBuf, generation: u64, tx: mpsc::Sender<Msg>) {
    std::thread::spawn(move || {
        let d = detail::inspect(&path);
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
