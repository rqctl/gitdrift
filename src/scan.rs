use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

use crate::config::Config;
use crate::discover::{self, Found, Problem};
use crate::status::{self, RepoStatus};

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    pub prune: Vec<String>,
    pub threads: usize,
}

impl ScanOptions {
    pub fn from_config(cfg: &Config) -> Self {
        ScanOptions {
            roots: cfg.roots.clone(),
            prune: cfg.prune.clone(),
            threads: num_cpus::get().max(1),
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Status(Box<RepoStatus>),
    Problem(Problem),
    Done,
}

/// Walk and inspect concurrently, emitting results as they land.
///
/// Discovery is far cheaper than inspection, so the work channel is bounded:
/// the walker must not race ahead and buffer every path in memory.
pub fn stream(opts: ScanOptions) -> Receiver<Event> {
    let (out_tx, out_rx) = unbounded::<Event>();
    std::thread::spawn(move || run(opts, out_tx));
    out_rx
}

fn run(opts: ScanOptions, out: Sender<Event>) {
    let threads = opts.threads.max(1);
    let (work_tx, work_rx) = bounded::<Found>(threads * 4);

    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let rx = work_rx.clone();
            let out = out.clone();
            std::thread::spawn(move || {
                for found in rx {
                    let st = status::inspect(&found.path, found.display_name);
                    if out.send(Event::Status(Box::new(st))).is_err() {
                        break; // the consumer went away
                    }
                }
            })
        })
        .collect();
    drop(work_rx);

    {
        let work_tx = Arc::new(work_tx);
        let tx = Arc::clone(&work_tx);
        let out_for_problems = out.clone();
        discover::walk(
            &opts.roots,
            &opts.prune,
            move |found| {
                let _ = tx.send(found);
            },
            move |problem| {
                let _ = out_for_problems.send(Event::Problem(problem));
            },
        );
        // Both Arc clones must die here or the workers never see the channel
        // close and the scan hangs forever.
    }

    for w in workers {
        let _ = w.join();
    }
    let _ = out.send(Event::Done);
}

/// Blocking scan. Returns rows already ordered by drift.
pub fn collect(opts: ScanOptions) -> (Vec<RepoStatus>, Vec<Problem>) {
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    for ev in stream(opts) {
        match ev {
            Event::Status(s) => rows.push(*s),
            Event::Problem(p) => problems.push(p),
            Event::Done => break,
        }
    }
    crate::drift::sort(&mut rows);
    (rows, problems)
}
