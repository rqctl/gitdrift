use crate::status::{Head, RepoStatus};

pub const W_ERROR: u32 = 2000;
pub const W_CONFLICT: u32 = 1000;
pub const W_DIVERGED: u32 = 500;
pub const W_AHEAD: u32 = 100;
pub const W_DIRTY: u32 = 50;
pub const W_DETACHED: u32 = 40;
pub const W_BEHIND: u32 = 25;
pub const W_NO_UPSTREAM: u32 = 20;
pub const W_UNTRACKED: u32 = 10;
pub const W_STASH: u32 = 5;

/// How far this repository has drifted. Higher sorts first.
pub fn score(s: &RepoStatus) -> u32 {
    if s.error.is_some() {
        return W_ERROR;
    }
    let mut total = 0;
    if s.conflicted > 0 {
        total += W_CONFLICT;
    }
    if s.ahead > 0 && s.behind > 0 {
        total += W_DIVERGED;
    }
    if s.ahead > 0 {
        total += W_AHEAD;
    }
    if s.staged > 0 || s.unstaged > 0 {
        total += W_DIRTY;
    }
    if matches!(s.head, Head::Detached(_)) {
        total += W_DETACHED;
    }
    if s.behind > 0 {
        total += W_BEHIND;
    }
    if s.upstream.is_none() && !matches!(s.head, Head::Unborn) {
        total += W_NO_UPSTREAM;
    }
    if s.untracked > 0 {
        total += W_UNTRACKED;
    }
    if s.stash_count > 0 {
        total += W_STASH;
    }
    total
}

pub fn is_drifted(s: &RepoStatus) -> bool {
    score(s) > 0
}

/// How the list is ordered. Cycled with `o`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Drift,
    Name,
    Recent,
    Stale,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Drift => "drift",
            Sort::Name => "name",
            Sort::Recent => "recently committed",
            Sort::Stale => "least recently fetched",
        }
    }

    pub fn next(self) -> Sort {
        match self {
            Sort::Drift => Sort::Name,
            Sort::Name => Sort::Recent,
            Sort::Recent => Sort::Stale,
            Sort::Stale => Sort::Drift,
        }
    }
}

pub fn sort_by(rows: &mut [RepoStatus], mode: Sort) {
    match mode {
        Sort::Drift => sort(rows),
        Sort::Name => rows.sort_by(|a, b| a.display_name.cmp(&b.display_name)),
        Sort::Recent => rows.sort_by(|a, b| {
            b.last_commit_time
                .cmp(&a.last_commit_time)
                .then_with(|| a.display_name.cmp(&b.display_name))
        }),
        // Never-fetched repos have no age at all; they sort last rather than
        // claiming to be infinitely stale.
        Sort::Stale => rows.sort_by(|a, b| {
            b.fetch_age
                .cmp(&a.fetch_age)
                .then_with(|| a.display_name.cmp(&b.display_name))
        }),
    }
}

/// Drifted first (by score), then most recently committed first.
pub fn sort(rows: &mut [RepoStatus]) {
    rows.sort_by(|a, b| {
        score(b)
            .cmp(&score(a))
            .then_with(|| b.last_commit_time.cmp(&a.last_commit_time))
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn row(name: &str) -> RepoStatus {
        RepoStatus {
            path: PathBuf::from("/tmp").join(name),
            display_name: name.to_string(),
            head: Head::Branch("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            stash_count: 0,
            last_commit_time: Some(1000),
            fetch_age: None,
            error: None,
        }
    }

    #[test]
    fn a_clean_in_sync_repo_scores_zero_and_is_not_drifted() {
        let r = row("clean");
        assert_eq!(score(&r), 0);
        assert!(!is_drifted(&r));
    }

    #[test]
    fn each_condition_contributes_its_documented_weight() {
        let mut r = row("x");
        r.conflicted = 3;
        assert_eq!(score(&r), 1000, "conflicts are flat-rated, not per-file");

        let mut r = row("x");
        r.ahead = 5;
        assert_eq!(score(&r), 100);

        let mut r = row("x");
        r.behind = 5;
        assert_eq!(score(&r), 25);

        let mut r = row("x");
        r.unstaged = 1;
        assert_eq!(score(&r), 50);

        let mut r = row("x");
        r.untracked = 1;
        assert_eq!(score(&r), 10);

        let mut r = row("x");
        r.stash_count = 1;
        assert_eq!(score(&r), 5);

        let mut r = row("x");
        r.head = Head::Detached("abc1234".into());
        assert_eq!(score(&r), 40);

        let mut r = row("x");
        r.upstream = None;
        assert_eq!(score(&r), 20);

        let mut r = row("x");
        r.error = Some("boom".into());
        assert_eq!(score(&r), 2000);
    }

    #[test]
    fn an_unborn_repo_is_not_penalised_for_having_no_upstream() {
        let mut r = row("fresh");
        r.head = Head::Unborn;
        r.upstream = None;
        assert_eq!(score(&r), 0);
    }

    #[test]
    fn diverged_adds_its_own_weight_on_top_of_ahead_and_behind() {
        let mut r = row("x");
        r.ahead = 1;
        r.behind = 1;
        assert_eq!(score(&r), 500 + 100 + 25);
    }

    #[test]
    fn weights_are_additive() {
        let mut r = row("x");
        r.ahead = 1;
        r.unstaged = 1;
        r.untracked = 1;
        assert_eq!(score(&r), 100 + 50 + 10);
    }

    #[test]
    fn sort_puts_drifted_first_then_breaks_ties_by_recency() {
        let mut clean_old = row("clean_old");
        clean_old.last_commit_time = Some(10);
        let mut clean_new = row("clean_new");
        clean_new.last_commit_time = Some(90);
        let mut dirty_old = row("dirty_old");
        dirty_old.unstaged = 1;
        dirty_old.last_commit_time = Some(10);
        let mut dirty_new = row("dirty_new");
        dirty_new.unstaged = 1;
        dirty_new.last_commit_time = Some(90);
        let mut ahead = row("ahead");
        ahead.ahead = 1;

        let mut rows = vec![clean_old, dirty_old, clean_new, ahead, dirty_new];
        sort(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.display_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["ahead", "dirty_new", "dirty_old", "clean_new", "clean_old"]
        );
    }

    #[test]
    fn rows_without_a_commit_time_sort_last_within_their_tier() {
        let mut with = row("with");
        with.last_commit_time = Some(5);
        let mut without = row("without");
        without.last_commit_time = None;
        let mut rows = vec![without, with];
        sort(&mut rows);
        assert_eq!(rows[0].display_name, "with");
    }
}
