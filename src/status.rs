use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gix::bstr::BString;
use gix::remote::Direction;
use gix::status::index_worktree::Item as IwItem;
use gix::status::Item;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum Head {
    Branch(String),
    Detached(String),
    Unborn,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub path: PathBuf,
    pub display_name: String,
    pub head: Head,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub stash_count: u32,
    /// Count of refs under `refs/heads`.
    pub local_branches: u32,
    /// Unix seconds of the HEAD commit.
    pub last_commit_time: Option<i64>,
    /// Age of `.git/FETCH_HEAD`, i.e. how stale ahead/behind may be.
    #[serde(skip)]
    pub fetch_age: Option<Duration>,
    pub error: Option<String>,
}

impl RepoStatus {
    fn blank(path: &Path, display_name: String) -> Self {
        RepoStatus {
            path: path.to_path_buf(),
            display_name,
            head: Head::Unborn,
            upstream: None,
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            stash_count: 0,
            local_branches: 0,
            last_commit_time: None,
            fetch_age: None,
            error: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.staged + self.unstaged + self.conflicted > 0
    }
}

/// Inspect one repository. Never panics; failures land in `error`.
pub fn inspect(path: &Path, display_name: String) -> RepoStatus {
    let mut out = RepoStatus::blank(path, display_name);
    if let Err(e) = fill(path, &mut out) {
        out.error = Some(e.to_string());
    }
    out
}

fn fill(path: &Path, out: &mut RepoStatus) -> anyhow::Result<()> {
    let repo = gix::open(path)?;

    let head = repo.head()?;
    out.head = if head.is_unborn() {
        Head::Unborn
    } else if head.is_detached() {
        Head::Detached(
            head.id()
                .map(|i| i.to_hex_with_len(7).to_string())
                .unwrap_or_default(),
        )
    } else {
        Head::Branch(
            head.referent_name()
                .map(|n| n.shorten().to_string())
                .unwrap_or_default(),
        )
    };

    if let Some(id) = head.id() {
        if let Ok(object) = id.object() {
            if let Ok(t) = object.into_commit().time() {
                out.last_commit_time = Some(t.seconds);
            }
        }
    }

    count_changes(&repo, out)?;
    count_ahead_behind(&repo, &head, out);
    out.stash_count = count_stashes(&repo);
    out.local_branches = count_local_branches(&repo);
    out.fetch_age = fetch_age(path);
    Ok(())
}

fn count_local_branches(repo: &gix::Repository) -> u32 {
    let Ok(refs) = repo.references() else {
        return 0;
    };
    let Ok(iter) = refs.local_branches() else {
        return 0;
    };
    iter.filter_map(Result::ok).count() as u32
}

fn count_changes(repo: &gix::Repository, out: &mut RepoStatus) -> anyhow::Result<()> {
    use gix::status::plumbing::index_as_worktree::EntryStatus;

    let iter = repo
        .status(gix::progress::Discard)?
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: true })
        .untracked_files(gix::status::UntrackedFiles::Files)
        // Files, not Collapsed: a collapsed directory is reported untracked
        // without recursing far enough to notice every file inside it is
        // ignored, which made clean repos look dirty. Matches `status -uall`.
        .into_iter(None::<BString>)?;

    for item in iter {
        match item? {
            Item::TreeIndex(_) => out.staged += 1,
            Item::IndexWorktree(iw) => match iw {
                IwItem::Modification { status, .. } => match status {
                    EntryStatus::Conflict { .. } => out.conflicted += 1,
                    _ => out.unstaged += 1,
                },
                IwItem::DirectoryContents { entry, .. } => {
                    if entry.status == gix::dir::entry::Status::Untracked {
                        out.untracked += 1;
                    }
                }
                IwItem::Rewrite { .. } => out.unstaged += 1,
            },
        }
    }
    Ok(())
}

fn count_ahead_behind(repo: &gix::Repository, head: &gix::Head<'_>, out: &mut RepoStatus) {
    let Some(reference) = head.clone().try_into_referent() else {
        return;
    };
    let Some(Ok(upstream_name)) = reference.remote_tracking_ref_name(Direction::Fetch) else {
        return;
    };
    out.upstream = Some(upstream_name.shorten().to_string());

    let local = reference.id().detach();
    let Ok(upstream_ref) = repo.find_reference(upstream_name.as_ref()) else {
        return; // configured upstream that was never fetched
    };
    let upstream = upstream_ref.id().detach();

    if let Ok(walk) = repo.rev_walk([local]).with_hidden([upstream]).all() {
        out.ahead = walk.count() as u32;
    }
    if let Ok(walk) = repo.rev_walk([upstream]).with_hidden([local]).all() {
        out.behind = walk.count() as u32;
    }
}

fn count_stashes(repo: &gix::Repository) -> u32 {
    let Ok(stash) = repo.find_reference("refs/stash") else {
        return 0;
    };
    stash
        .log_iter()
        .all()
        .ok()
        .flatten()
        .map(|iter| iter.count() as u32)
        .unwrap_or(0)
}

fn fetch_age(path: &Path) -> Option<Duration> {
    let meta = std::fs::metadata(path.join(".git/FETCH_HEAD")).ok()?;
    SystemTime::now().duration_since(meta.modified().ok()?).ok()
}
