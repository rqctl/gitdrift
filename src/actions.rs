use std::path::Path;
use std::process::Command;

use crate::status::RepoStatus;

#[derive(Debug)]
pub enum ActionError {
    Dirty,
    NoUpstream,
    Git(String),
    Spawn(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Dirty => write!(f, "worktree has uncommitted changes"),
            ActionError::NoUpstream => write!(f, "branch has no upstream"),
            ActionError::Git(msg) => write!(f, "git: {msg}"),
            ActionError::Spawn(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ActionError {}

/// Judged from the last known status; divergence is left to `pull` itself,
/// which defers to the user's own `pull.rebase`/`pull.ff` config.
pub fn can_pull(s: &RepoStatus) -> Result<(), ActionError> {
    if s.upstream.is_none() {
        return Err(ActionError::NoUpstream);
    }
    if s.is_dirty() {
        return Err(ActionError::Dirty);
    }
    Ok(())
}

fn run_git(path: &Path, args: &[&str]) -> Result<String, ActionError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|e| ActionError::Spawn(format!("could not run git: {e}")))?;
    if out.status.success() {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(text.trim().to_string())
    } else {
        // Some failures (merge conflicts, overwritten changes) only reach stdout.
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        Err(ActionError::Git(summarize(&text)))
    }
}

/// Reduces git's output to the one line a toast can show. Git buries the cause
/// under progress, `From <url>` and `hint:` lines, and the footer cannot wrap,
/// so only the first two lines that carry information are kept.
fn summarize(stderr: &str) -> String {
    if stderr.contains("divergent branches") {
        return "the branch has diverged from its upstream: set pull.rebase or pull.ff, or resolve it in a shell".into();
    }
    let all: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("hint:") && !l.starts_with("From "))
        .collect();
    // Ref-update lines from a fetch would otherwise crowd out the cause.
    let causes: Vec<&str> = all
        .iter()
        .copied()
        .filter(|l| {
            ["fatal: ", "error: ", "CONFLICT", "Automatic merge failed"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .collect();
    let lines: Vec<&str> = if causes.is_empty() { all } else { causes }
        .into_iter()
        .map(|l| {
            l.strip_prefix("fatal: ")
                .or_else(|| l.strip_prefix("error: "))
                .unwrap_or(l)
        })
        .take(2)
        .collect();
    if lines.is_empty() {
        "failed without any output from git".into()
    } else {
        lines.join(" · ")
    }
}

pub fn fetch(path: &Path) -> Result<String, ActionError> {
    run_git(path, &["fetch", "--prune", "--quiet"])
}

/// Move HEAD to an existing local branch. Non-destructive: git refuses rather
/// than losing work, and the caller has already rejected a dirty worktree.
pub fn switch_branch(path: &Path, branch: &str) -> Result<String, ActionError> {
    run_git(path, &["switch", branch])
}

/// Defers to the repository's own `pull.rebase`/`pull.ff` config, same as
/// running `git pull` by hand — gitdrift does not force fast-forward-only.
pub fn pull(path: &Path, s: &RepoStatus) -> Result<String, ActionError> {
    can_pull(s)?;
    run_git(path, &["pull", "--quiet"])
}

/// Local branches in `git branch -vv` output whose upstream is gone.
///
/// The current branch (`*`) and branches checked out in another worktree
/// (`+`) are skipped: git refuses to delete either.
pub fn gone_branches(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter(|l| !l.starts_with('*') && !l.starts_with('+'))
        .filter_map(|l| {
            // name, sha, then `[upstream: gone]` if and only if it is gone.
            // Matching ": gone]" anywhere would let a commit message name a
            // branch for deletion.
            let mut fields = l.split_whitespace();
            let name = fields.next()?;
            let _sha = fields.next()?;
            let upstream = fields.next()?;
            let gone = upstream.starts_with('[')
                && upstream.ends_with(':')
                && fields.next() == Some("gone]");
            gone.then(|| name.to_string())
        })
        .collect()
}

/// Prune stale remote-tracking refs, then delete local branches left without
/// an upstream. Mirrors the user's `gpc` alias, minus the pull.
///
/// Irreversible: the caller must have confirmed with the user first. `-D`
/// rather than `-d` because an upstream that is gone leaves git unable to
/// prove the branch was merged.
/// Irreversible, confirmed by the caller already. `-D` not `-d`: an unmerged
/// branch must not silently block a delete the user already agreed to.
pub fn delete_branch(path: &Path, branch: &str) -> Result<String, ActionError> {
    run_git(path, &["branch", "-D", branch])
}

pub fn prune_gone_branches(path: &Path) -> Result<String, ActionError> {
    run_git(
        path,
        &["fetch", "--prune", "--prune-tags", "--force", "--quiet"],
    )?;
    let gone = gone_branches(&run_git(path, &["branch", "-vv"])?);
    if gone.is_empty() {
        return Ok("no branches to prune".to_string());
    }
    let mut args = vec!["branch", "-D"];
    args.extend(gone.iter().map(String::as_str));
    run_git(path, &args)?;
    Ok(format!("deleted {} branches", gone.len()))
}

/// Inherits the terminal; caller must leave raw mode first and restore it after.
fn run_interactive(path: &Path, program: &str, args: &[&str]) -> Result<(), ActionError> {
    Command::new(program)
        .args(args)
        .current_dir(path)
        .status()
        .map_err(|e| ActionError::Spawn(format!("could not run {program}: {e}")))?;
    Ok(())
}

pub fn open_shell(path: &Path) -> Result<(), ActionError> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    run_interactive(path, &shell, &[])
}

/// The browsable `https://host/path` for a git remote URL, or `None` for
/// anything without a host (a local path, `file://`). The repository root is
/// the same on every forge, so no forge needs recognising.
pub fn to_web_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (scheme, host_and_path) = match remote.split_once("://") {
        Some(("file", _)) => return None,
        Some((scheme, rest)) => {
            let rest = rest
                .split_once('@')
                .filter(|(u, _)| !u.contains('/'))
                .map_or(rest, |(_, r)| r);
            let (authority, path) = rest.split_once('/')?;
            // Only an http(s) remote's port is a web port; an ssh port is not.
            let web_scheme = if scheme == "http" { "http" } else { "https" };
            let host = if scheme == "http" || scheme == "https" {
                authority
            } else {
                authority.split(':').next().unwrap_or(authority)
            };
            (web_scheme, format!("{host}/{path}"))
        }
        None => {
            // scp-like: [user@]host:path
            let rest = remote.split_once('@').map_or(remote, |(_, r)| r);
            let (host, path) = rest.split_once(':')?;
            if host.is_empty() || host.contains('/') {
                return None;
            }
            ("https", format!("{host}/{}", path.trim_start_matches('/')))
        }
    };
    let trimmed = host_and_path.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    Some(format!("{scheme}://{trimmed}"))
}

/// The web page of the remote a repository's branch tracks, else `origin`,
/// else its only remote. `git remote get-url` applies `insteadOf` rewrites.
pub fn web_url(path: &Path, branch: Option<&str>) -> Result<String, ActionError> {
    let tracked = branch
        .and_then(|b| run_git(path, &["config", "--get", &format!("branch.{b}.remote")]).ok())
        .filter(|r| !r.is_empty() && r != ".");
    let remote = match tracked {
        Some(r) => r,
        None => {
            let remotes = run_git(path, &["remote"])?;
            match remotes
                .lines()
                .find(|r| *r == "origin")
                .or(remotes.lines().next())
            {
                Some(name) => name.to_string(),
                None => return Err(ActionError::Git("no remote configured".into())),
            }
        }
    };
    let raw = run_git(path, &["remote", "get-url", &remote])?;
    to_web_url(&raw).ok_or_else(|| ActionError::Git(format!("{remote} is not a web remote: {raw}")))
}

/// Hands the URL to git's own browser launcher, which honours `web.browser`,
/// `browser.<tool>.path` and `$BROWSER`.
pub fn open_on_web(path: &Path, branch: Option<&str>) -> Result<String, ActionError> {
    let url = web_url(path, branch)?;
    let mut child = Command::new("git")
        .args(["web--browse", &url])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| ActionError::Spawn(format!("could not run git web--browse: {e}")))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(url)
}

pub fn lazygit_on_path() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join("lazygit").is_file()))
}

pub fn open_lazygit(path: &Path) -> Result<(), ActionError> {
    run_interactive(path, "lazygit", &["-p", &path.to_string_lossy()])
}

/// The pager these full-screen views need. A global `core.pager=` (empty) is a
/// deliberate "never page" for ordinary git use, but here it would dump the
/// output and return before the TUI had even left the screen, so gitdrift
/// supplies its own. `GIT_PAGER` still wins: git resolves it ahead of `-c`.
pub fn pager() -> String {
    std::env::var("GIT_PAGER")
        .or_else(|_| std::env::var("PAGER"))
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "less -R".to_string())
}

fn run_git_interactive(path: &Path, args: &[&str]) -> Result<(), ActionError> {
    let pager = format!("core.pager={}", pager());
    let mut all = vec!["-c", &pager, "--paginate"];
    all.extend_from_slice(args);
    run_interactive(path, "git", &all)
}

pub fn view_diff(path: &Path) -> Result<(), ActionError> {
    run_git_interactive(path, &["diff", "HEAD...@{u}"])
}

/// The diff an MR would show: everything on `target` since it forked from `base`.
fn view_ref_diff(path: &Path, base: &str, target: &str) -> Result<(), ActionError> {
    run_git_interactive(path, &["diff", &format!("{base}...{target}")])
}

pub fn view_ancestor_diff(path: &Path, base: &str) -> Result<(), ActionError> {
    view_ref_diff(path, base, "HEAD")
}

/// Diffs a branch that need not be checked out — picked from the branch popover.
pub fn view_branch_diff(path: &Path, base: &str, branch: &str) -> Result<(), ActionError> {
    view_ref_diff(path, base, branch)
}

/// Two dots: the commits that are incoming, not a symmetric difference.
pub fn view_log(path: &Path) -> Result<(), ActionError> {
    run_git_interactive(path, &["log", "--stat", "HEAD..@{u}"])
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_become_web_urls() {
        for (remote, web) in [
            ("git@github.com:me/repo.git", "https://github.com/me/repo"),
            ("git@gitlab.corp:a/b/c.git", "https://gitlab.corp/a/b/c"),
            (
                "ssh://git@gitlab.corp:2222/a/b.git",
                "https://gitlab.corp/a/b",
            ),
            ("ssh://gitlab.corp/a/b", "https://gitlab.corp/a/b"),
            (
                "https://user@gitlab.corp:8443/a/b.git",
                "https://gitlab.corp:8443/a/b",
            ),
            ("http://gitlab.local/a/b.git/", "http://gitlab.local/a/b"),
            ("git://example.org/a/b.git", "https://example.org/a/b"),
        ] {
            assert_eq!(to_web_url(remote).as_deref(), Some(web), "{remote}");
        }
    }

    #[test]
    fn local_remotes_have_no_web_page() {
        for remote in ["/srv/git/repo.git", "../repo", "file:///srv/repo.git", ""] {
            assert_eq!(to_web_url(remote), None, "{remote}");
        }
    }

    #[test]
    fn divergent_branches_get_actionable_advice_on_one_line() {
        let err = "hint: x\nfatal: Need to specify how to reconcile divergent branches.\n";
        let msg = summarize(err);
        assert!(msg.starts_with("the branch has diverged"), "{msg}");
        assert!(!msg.contains('\n'));
    }

    #[test]
    fn a_conflict_is_not_crowded_out_by_fetch_progress() {
        let err = "From host:x/y\n   abc..def  main -> origin/main\nCONFLICT (content): Merge conflict in a\nAutomatic merge failed; fix conflicts\n";
        assert_eq!(
            summarize(err),
            "CONFLICT (content): Merge conflict in a · Automatic merge failed; fix conflicts"
        );
    }

    #[test]
    fn summarize_drops_noise_and_joins_what_is_left() {
        let err = "From host:x/y\nhint: do something\nfatal: refusing to fetch into branch 'refs/heads/a' checked out at '/w/a'\n";
        assert_eq!(
            summarize(err),
            "refusing to fetch into branch 'refs/heads/a' checked out at '/w/a'"
        );
        assert_eq!(summarize("\n"), "failed without any output from git");
    }

    #[test]
    fn gone_branches_picks_only_the_ones_whose_upstream_disappeared() {
        let listing = "\
* main       1a2b3c4 [origin/main] latest
  feat/old   5d6e7f8 [origin/feat/old: gone] older work
  feat/live  9a8b7c6 [origin/feat/live: ahead 2] in progress
  no-remote  1122334 local only
";
        assert_eq!(gone_branches(listing), vec!["feat/old".to_string()]);
    }

    #[test]
    fn the_current_branch_is_never_offered_for_deletion() {
        let listing = "* main 1a2b3c4 [origin/main: gone] orphaned\n";
        assert!(gone_branches(listing).is_empty());
    }

    #[test]
    fn a_commit_message_cannot_nominate_a_branch_for_deletion() {
        let listing = "  keep 1a2b3c4 [origin/keep] chore: drop the [thing: gone] marker\n";
        assert!(
            gone_branches(listing).is_empty(),
            "only the upstream field counts"
        );
    }

    #[test]
    fn a_branch_with_no_upstream_at_all_is_left_alone() {
        let listing = "  local 1a2b3c4 some commit message\n";
        assert!(gone_branches(listing).is_empty());
    }

    #[test]
    fn a_branch_checked_out_in_another_worktree_is_skipped() {
        let listing = "+ wip 1a2b3c4 [origin/wip: gone] elsewhere\n";
        assert!(gone_branches(listing).is_empty());
    }
}
