use std::path::Path;
use std::process::Command;

use gix::bstr::BString;
use gix::remote::Direction;
use gix::status::index_worktree::Item as IwItem;
use gix::status::Item;

use crate::theme::Facet;

const RECENT_LIMIT: usize = 10;
const CHANGED_LIMIT: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct RepoDetail {
    pub branches: Vec<BranchInfo>,
    pub stashes: Vec<StashEntry>,
    pub remotes: Vec<RemoteInfo>,
    pub recent: Vec<CommitInfo>,
    pub changed_files: Vec<(Facet, String)>,
    pub incoming: Option<Incoming>,
}

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

#[derive(Debug, Clone)]
pub struct BranchInfo {
    pub name: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub is_head: bool,
}

#[derive(Debug, Clone)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub short_id: String,
    pub summary: String,
    pub author: String,
    pub time: Option<i64>,
}

/// Everything shown in the detail pane. Best-effort: a failure in one section
/// leaves that section empty rather than losing the whole pane.
pub fn inspect(path: &Path) -> RepoDetail {
    let mut out = RepoDetail::default();
    let Ok(repo) = gix::open(path) else {
        return out;
    };

    let head_name = repo
        .head()
        .ok()
        .and_then(|h| h.referent_name().map(|n| n.shorten().to_string()));

    out.branches = branches(&repo, head_name.as_deref());
    out.stashes = stashes(&repo);
    out.remotes = remotes(&repo);
    out.recent = recent(&repo);
    out.changed_files = changed_files(&repo);

    let behind = out
        .branches
        .iter()
        .find(|b| b.is_head)
        .map_or(0, |b| b.behind);
    if behind > 0 {
        out.incoming = incoming(path);
    }
    out
}

fn branches(repo: &gix::Repository, head_name: Option<&str>) -> Vec<BranchInfo> {
    let Ok(platform) = repo.references() else {
        return Vec::new();
    };
    let Ok(iter) = platform.local_branches() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for reference in iter.flatten() {
        let name = reference.name().shorten().to_string();
        let mut info = BranchInfo {
            is_head: head_name == Some(name.as_str()),
            name,
            upstream: None,
            ahead: 0,
            behind: 0,
        };
        if let Some(Ok(up)) = reference.remote_tracking_ref_name(Direction::Fetch) {
            info.upstream = Some(up.shorten().to_string());
            if let (Some(local), Ok(up_ref)) =
                (reference.try_id(), repo.find_reference(up.as_ref()))
            {
                let local = local.detach();
                let upstream = up_ref.id().detach();
                if let Ok(w) = repo.rev_walk([local]).with_hidden([upstream]).all() {
                    info.ahead = w.count() as u32;
                }
                if let Ok(w) = repo.rev_walk([upstream]).with_hidden([local]).all() {
                    info.behind = w.count() as u32;
                }
            }
        }
        out.push(info);
    }
    out.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));
    out
}

fn stashes(repo: &gix::Repository) -> Vec<StashEntry> {
    let Ok(stash) = repo.find_reference("refs/stash") else {
        return Vec::new();
    };
    let mut log_iter = stash.log_iter();
    let Ok(Some(iter)) = log_iter.all() else {
        return Vec::new();
    };
    // The reflog iterates oldest first; `git stash list` shows newest first.
    let mut messages: Vec<String> = iter
        .filter_map(|line| Some(line.ok()?.message.to_string()))
        .collect();
    messages.reverse();
    messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| StashEntry { index, message })
        .collect()
}

fn remotes(repo: &gix::Repository) -> Vec<RemoteInfo> {
    repo.remote_names()
        .into_iter()
        .filter_map(|name| {
            let name = name.to_string();
            let remote = repo.find_remote(name.as_str()).ok()?;
            let url = remote
                .url(Direction::Fetch)
                .map(|u| u.to_bstring().to_string())
                .unwrap_or_default();
            Some(RemoteInfo { name, url })
        })
        .collect()
}

fn recent(repo: &gix::Repository) -> Vec<CommitInfo> {
    let Ok(head) = repo.head() else {
        return Vec::new();
    };
    let Some(id) = head.id() else {
        return Vec::new(); // unborn
    };
    let Ok(walk) = repo.rev_walk([id.detach()]).all() else {
        return Vec::new();
    };
    walk.take(RECENT_LIMIT)
        .filter_map(|info| {
            let info = info.ok()?;
            let commit = info.object().ok()?;
            Some(CommitInfo {
                short_id: info.id().to_hex_with_len(7).to_string(),
                summary: commit.message().ok()?.summary().to_string(),
                author: commit
                    .author()
                    .ok()
                    .map(|a| a.name.to_string())
                    .unwrap_or_default(),
                time: commit.time().ok().map(|t| t.seconds),
            })
        })
        .collect()
}

fn changed_files(repo: &gix::Repository) -> Vec<(Facet, String)> {
    use gix::status::plumbing::index_as_worktree::EntryStatus;

    let Ok(platform) = repo.status(gix::progress::Discard) else {
        return Vec::new();
    };
    let Ok(iter) = platform
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: true })
        .untracked_files(gix::status::UntrackedFiles::Files)
        .into_iter(None::<BString>)
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in iter.flatten() {
        if out.len() >= CHANGED_LIMIT {
            break;
        }
        match item {
            Item::TreeIndex(change) => out.push((Facet::Staged, change.location().to_string())),
            Item::IndexWorktree(iw) => match iw {
                IwItem::Modification {
                    rela_path, status, ..
                } => {
                    let facet = match status {
                        EntryStatus::Conflict { .. } => Facet::Conflict,
                        _ => Facet::Unstaged,
                    };
                    out.push((facet, rela_path.to_string()));
                }
                IwItem::DirectoryContents { entry, .. } => {
                    if entry.status == gix::dir::entry::Status::Untracked {
                        out.push((Facet::Untracked, entry.rela_path.to_string()));
                    }
                }
                IwItem::Rewrite { dirwalk_entry, .. } => {
                    out.push((Facet::Unstaged, dirwalk_entry.rela_path.to_string()));
                }
            },
        }
    }
    out
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

    // `--short` only accepts a single rev at a time, hence two calls; either
    // one failing means the whole section is unreliable, so bail like `diff` does.
    let head = git_stdout(path, &["rev-parse", "--short", "HEAD"])?;
    let upstream = git_stdout(path, &["rev-parse", "--short", "@{u}"])?;
    let range = format!("{}..{}", head.trim(), upstream.trim());

    Some(Incoming {
        files,
        truncated,
        total_added,
        total_removed,
        range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_lines_become_deltas() {
        let out = "42\t17\tsrc/api/users.rs\n8\t0\tsrc/db/schema.sql\n";
        assert_eq!(
            parse_numstat(out),
            vec![
                FileDelta {
                    added: 42,
                    removed: 17,
                    path: "src/api/users.rs".into()
                },
                FileDelta {
                    added: 8,
                    removed: 0,
                    path: "src/db/schema.sql".into()
                },
            ]
        );
    }

    #[test]
    fn a_binary_file_counts_as_no_lines_either_way() {
        let out = "-\t-\tlogo.png\n";
        assert_eq!(
            parse_numstat(out),
            vec![FileDelta {
                added: 0,
                removed: 0,
                path: "logo.png".into()
            }]
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
