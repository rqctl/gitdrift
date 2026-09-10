mod support;

use gitdrift::status::{inspect, Head, RepoStatus};
use support::TestRepo;

fn s(r: &TestRepo) -> RepoStatus {
    inspect(r.path(), "fixture".to_string())
}

#[test]
fn clean_repo_reports_branch_and_no_changes() {
    let r = TestRepo::new();
    r.commit("a.txt", "one", "one");
    let st = s(&r);
    assert_eq!(st.head, Head::Branch("main".into()));
    assert_eq!(
        (st.staged, st.unstaged, st.untracked, st.conflicted),
        (0, 0, 0, 0)
    );
    assert_eq!(st.upstream, None);
    assert_eq!(st.stash_count, 0);
    assert!(st.error.is_none(), "error = {:?}", st.error);
    assert!(st.last_commit_time.is_some());
}

#[test]
fn counts_staged_unstaged_and_untracked_separately() {
    let r = TestRepo::new();
    r.commit("tracked.txt", "one", "one");
    r.write("tracked.txt", "modified"); // unstaged
    r.write("staged.txt", "new");
    r.git(&["add", "staged.txt"]); // staged
    r.write("loose.txt", "loose"); // untracked
    let st = s(&r);
    assert_eq!(st.staged, 1, "staged");
    assert_eq!(st.unstaged, 1, "unstaged");
    assert_eq!(st.untracked, 1, "untracked");
}

#[test]
fn gitignored_files_are_not_untracked() {
    let r = TestRepo::new();
    r.commit(".gitignore", "ignored/\n", "ignore");
    r.write("ignored/junk.txt", "junk");
    assert_eq!(s(&r).untracked, 0);
}

#[test]
fn a_directory_holding_only_ignored_content_is_not_untracked() {
    // Regression. The dirwalk used to collapse such a directory into a single
    // "untracked" entry without recursing far enough to notice everything in it
    // was ignored, so a clean repo reported as dirty. The empty subdirectory is
    // load-bearing: without it gix resolves the directory correctly and the bug
    // does not appear.
    let r = TestRepo::new();
    r.commit(".gitignore", "tool/local.json\n", "ignore one file");
    r.write("tool/local.json", "{}");
    std::fs::create_dir_all(r.path().join("tool/empty")).unwrap();

    assert_eq!(
        r.git(&["status", "--porcelain"]).trim(),
        "",
        "git sees nothing"
    );
    assert_eq!(s(&r).untracked, 0, "and neither should we");
}

#[test]
fn an_excludes_file_outside_the_repo_is_honoured() {
    // The real-world trigger was a *global* excludes file (~/.config/git/ignore).
    // core.excludesFile exercises the same resolution path without touching the
    // developer's own global config.
    let r = TestRepo::new();
    r.commit("a.txt", "one", "one");
    let excludes = r.aux.path().join("excludes");
    std::fs::write(&excludes, "**/.tool/local.json\n").unwrap();
    r.git(&["config", "core.excludesFile", excludes.to_str().unwrap()]);
    r.write(".tool/local.json", "{}");
    std::fs::create_dir_all(r.path().join(".tool/empty")).unwrap();

    assert_eq!(
        r.git(&["status", "--porcelain"]).trim(),
        "",
        "git sees nothing"
    );
    assert_eq!(s(&r).untracked, 0, "and neither should we");
}

#[test]
fn counts_conflicts() {
    let r = TestRepo::new();
    r.commit("f.txt", "base\n", "base");
    r.git(&["checkout", "-qb", "other"]);
    r.commit("f.txt", "other\n", "other");
    r.git(&["checkout", "-q", "main"]);
    r.commit("f.txt", "main\n", "main");
    // A conflicting merge is expected to fail, so don't assert on its status.
    let out = std::process::Command::new("git")
        .args(["merge", "other"])
        .current_dir(r.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "the merge was supposed to conflict");
    assert!(s(&r).conflicted >= 1, "expected a conflicted entry");
}

#[test]
fn reports_ahead_and_behind_against_upstream() {
    let r = TestRepo::new();
    r.commit("a.txt", "one", "one");
    let up = r.with_upstream();

    r.commit("b.txt", "two", "two");
    r.commit("c.txt", "three", "three");
    let _clone = r.advance_upstream(&up, "d.txt");
    r.git(&["fetch", "-q", "origin"]);

    let st = s(&r);
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!(st.ahead, 2, "ahead");
    assert_eq!(st.behind, 1, "behind");
    assert!(
        st.fetch_age.is_some(),
        "FETCH_HEAD should exist after a fetch"
    );
}

#[test]
fn counts_stashes() {
    let r = TestRepo::new();
    r.commit("a.txt", "one", "one");
    r.write("a.txt", "two");
    r.git(&["stash", "-q"]);
    r.write("a.txt", "three");
    r.git(&["stash", "-q"]);
    assert_eq!(s(&r).stash_count, 2);
}

#[test]
fn detects_detached_head() {
    let r = TestRepo::new();
    r.commit("a.txt", "one", "one");
    r.commit("b.txt", "two", "two");
    r.git(&["checkout", "-q", "HEAD~1"]);
    match s(&r).head {
        Head::Detached(sha) => assert_eq!(sha.len(), 7, "short sha, got {sha:?}"),
        other => panic!("expected detached, got {other:?}"),
    }
}

#[test]
fn detects_unborn_head() {
    let r = TestRepo::new();
    let st = s(&r);
    assert_eq!(st.head, Head::Unborn);
    assert!(st.error.is_none());
    assert_eq!(st.last_commit_time, None);
}

#[test]
fn a_non_repository_becomes_an_error_row_not_a_panic() {
    let d = tempfile::tempdir().unwrap();
    let st = inspect(d.path(), "notarepo".to_string());
    assert!(st.error.is_some());
    assert_eq!(st.display_name, "notarepo");
}
