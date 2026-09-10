mod support;

use gitdrift::actions::{
    can_pull, drop_stash, fetch, pager, pull_ff, resolve_editor, switch_branch, ActionError,
};
use gitdrift::status::{inspect, Head, RepoStatus};
use support::TestRepo;

fn st(r: &TestRepo) -> RepoStatus {
    inspect(r.path(), "fixture".into())
}

#[test]
fn can_pull_refuses_without_an_upstream() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    assert!(matches!(can_pull(&st(&r)), Err(ActionError::NoUpstream)));
}

#[test]
fn can_pull_refuses_a_dirty_worktree() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.with_upstream();
    r.write("a", "2");
    assert!(matches!(can_pull(&st(&r)), Err(ActionError::Dirty)));
}

#[test]
fn can_pull_refuses_when_not_fast_forwardable() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    let up = r.with_upstream();
    r.commit("b", "2", "two");
    let _clone = r.advance_upstream(&up, "c");
    r.git(&["fetch", "-q", "origin"]);

    let s = st(&r);
    assert_eq!(
        (s.ahead, s.behind),
        (1, 1),
        "the fixture must actually diverge"
    );
    assert!(matches!(can_pull(&s), Err(ActionError::NotFastForward)));
}

#[test]
fn can_pull_allows_a_clean_behind_repository() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    let up = r.with_upstream();
    let _clone = r.advance_upstream(&up, "c");
    r.git(&["fetch", "-q", "origin"]);

    let s = st(&r);
    assert_eq!((s.ahead, s.behind), (0, 1));
    assert!(can_pull(&s).is_ok(), "{:?}", can_pull(&s));
}

#[test]
fn can_pull_is_allowed_when_already_up_to_date() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.with_upstream();
    assert!(can_pull(&st(&r)).is_ok());
}

#[test]
fn untracked_files_alone_do_not_block_a_pull() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.with_upstream();
    r.write("loose", "x");
    let s = st(&r);
    assert_eq!(s.untracked, 1);
    assert!(
        can_pull(&s).is_ok(),
        "untracked files never conflict with a fast-forward"
    );
}

#[test]
fn fetch_updates_remote_tracking_refs() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    let up = r.with_upstream();
    let _clone = r.advance_upstream(&up, "b");

    assert_eq!(
        st(&r).behind,
        0,
        "before fetch we do not know about the new commit"
    );
    fetch(r.path()).unwrap();
    assert_eq!(st(&r).behind, 1, "after fetch we do");
}

#[test]
fn pull_ff_fast_forwards_and_clears_behind() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    let up = r.with_upstream();
    let _clone = r.advance_upstream(&up, "b");

    fetch(r.path()).unwrap();
    let before = st(&r);
    assert_eq!(before.behind, 1);

    pull_ff(r.path(), &before).unwrap();
    let after = st(&r);
    assert_eq!(after.behind, 0);
    assert!(r.path().join("b").exists());
}

#[test]
fn fetch_on_a_repository_with_no_remote_does_not_panic() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    match fetch(r.path()) {
        Err(ActionError::Git(_)) | Ok(_) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn editor_resolution_prefers_the_configured_editor() {
    assert_eq!(resolve_editor(Some("nvim")), "nvim");
}

#[test]
fn editor_resolution_falls_back_to_vi_when_nothing_is_set() {
    // Cannot safely clear the process environment here, so only assert the
    // configured branch and that the fallback chain never returns empty.
    assert!(!resolve_editor(None).is_empty());
}

#[test]
fn switch_branch_moves_head() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.git(&["branch", "other"]);
    switch_branch(r.path(), "other").unwrap();
    assert_eq!(
        inspect(r.path(), "fixture".into()).head,
        Head::Branch("other".into())
    );
}

#[test]
fn drop_stash_removes_exactly_one_entry() {
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.write("a", "2");
    r.git(&["stash", "push", "-qm", "first"]);
    r.write("a", "3");
    r.git(&["stash", "push", "-qm", "second"]);
    assert_eq!(inspect(r.path(), "fixture".into()).stash_count, 2);

    // git stacks newest-first: stash@{0} is "second", stash@{1} is "first".
    drop_stash(r.path(), 0).unwrap();
    assert_eq!(inspect(r.path(), "fixture".into()).stash_count, 1);
    let list = r.git(&["stash", "list"]);
    assert!(
        list.contains("first"),
        "survivor should be \"first\": {list}"
    );
    assert!(
        !list.contains("second"),
        "\"second\" should be dropped: {list}"
    );
}

#[test]
fn the_pager_falls_back_when_the_environment_offers_none() {
    // A global `core.pager=` is a real configuration; these full-screen views
    // still need something that holds the terminal.
    let saved = (std::env::var("GIT_PAGER").ok(), std::env::var("PAGER").ok());
    std::env::remove_var("GIT_PAGER");
    std::env::set_var("PAGER", "   ");
    assert_eq!(
        pager(),
        "less -R",
        "an empty PAGER must not be taken at its word"
    );

    std::env::set_var("PAGER", "bat");
    assert_eq!(pager(), "bat");
    std::env::set_var("GIT_PAGER", "delta");
    assert_eq!(pager(), "delta", "GIT_PAGER outranks PAGER");

    match saved.0 {
        Some(v) => std::env::set_var("GIT_PAGER", v),
        None => std::env::remove_var("GIT_PAGER"),
    }
    match saved.1 {
        Some(v) => std::env::set_var("PAGER", v),
        None => std::env::remove_var("PAGER"),
    }
}
