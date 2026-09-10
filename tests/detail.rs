mod support;

use gitdrift::detail::inspect;
use support::TestRepo;

#[test]
fn lists_local_branches_and_marks_head() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.git(&["branch", "feature"]);
    let d = inspect(r.path());
    let names: Vec<&str> = d.branches.iter().map(|b| b.name.as_str()).collect();
    assert!(names.contains(&"main"), "{names:?}");
    assert!(names.contains(&"feature"), "{names:?}");
    assert_eq!(d.branches.iter().filter(|b| b.is_head).count(), 1);
    assert!(
        d.branches
            .iter()
            .find(|b| b.name == "main")
            .unwrap()
            .is_head
    );
    assert_eq!(d.branches[0].name, "main", "HEAD sorts first");
}

#[test]
fn branch_rows_carry_ahead_and_behind_against_their_upstream() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    let up = r.with_upstream();
    r.commit("b", "2", "two");
    let _clone = r.advance_upstream(&up, "c");
    r.git(&["fetch", "-q", "origin"]);

    let d = inspect(r.path());
    let main = d.branches.iter().find(|b| b.name == "main").unwrap();
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!(main.ahead, 1);
    assert_eq!(main.behind, 1);
}

#[test]
fn lists_stashes_newest_first_with_their_messages() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.write("a", "2");
    r.git(&["stash", "push", "-q", "-m", "first"]);
    r.write("a", "3");
    r.git(&["stash", "push", "-q", "-m", "second"]);

    let d = inspect(r.path());
    assert_eq!(d.stashes.len(), 2);
    assert_eq!(d.stashes[0].index, 0);
    assert!(
        d.stashes[0].message.contains("second"),
        "{:?}",
        d.stashes[0].message
    );
    assert!(d.stashes[1].message.contains("first"));
}

#[test]
fn lists_remotes_with_urls() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.git(&["remote", "add", "origin", "https://example.com/x.git"]);
    let d = inspect(r.path());
    assert_eq!(d.remotes.len(), 1);
    assert_eq!(d.remotes[0].name, "origin");
    assert!(d.remotes[0].url.contains("example.com"));
}

#[test]
fn lists_recent_commits_newest_first_capped_at_ten() {
    let r = TestRepo::new();
    for n in 0..15 {
        r.commit("a", &format!("{n}"), &format!("commit {n}"));
    }
    let d = inspect(r.path());
    assert_eq!(d.recent.len(), 10);
    assert!(
        d.recent[0].summary.contains("commit 14"),
        "{:?}",
        d.recent[0].summary
    );
    assert_eq!(d.recent[0].short_id.len(), 7);
    assert!(d.recent[0].time.is_some());
    assert_eq!(d.recent[0].author, "Test");
}

#[test]
fn lists_changed_files_with_their_facet() {
    use gitdrift::theme::Facet;
    let r = TestRepo::new();
    r.commit("tracked", "1", "one");
    r.write("tracked", "2");
    r.write("loose", "x");
    let d = inspect(r.path());
    let names: Vec<&str> = d.changed_files.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"tracked"), "{names:?}");
    assert!(names.contains(&"loose"), "{names:?}");
    assert_eq!(
        d.changed_files
            .iter()
            .find(|(_, n)| n == "loose")
            .unwrap()
            .0,
        Facet::Untracked
    );
    assert_eq!(
        d.changed_files
            .iter()
            .find(|(_, n)| n == "tracked")
            .unwrap()
            .0,
        Facet::Unstaged
    );
}

#[test]
fn a_broken_repository_yields_empty_detail_rather_than_panicking() {
    let d = tempfile::tempdir().unwrap();
    let out = inspect(d.path());
    assert!(out.branches.is_empty());
    assert!(out.recent.is_empty());
}

#[test]
fn an_unborn_repository_yields_no_commits_and_does_not_panic() {
    let r = TestRepo::new();
    let d = inspect(r.path());
    assert!(d.recent.is_empty());
}

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
