mod support;

use gitdrift::scan::{collect, stream, Event, ScanOptions};
use support::{copy_tree, TestRepo};

fn opts(root: &std::path::Path) -> ScanOptions {
    ScanOptions {
        roots: vec![root.to_path_buf()],
        prune: gitdrift::config::DEFAULT_PRUNE
            .iter()
            .map(|s| s.to_string())
            .collect(),
        threads: 4,
    }
}

#[test]
fn collects_every_repository_under_the_root_sorted_by_drift() {
    let base = tempfile::tempdir().unwrap();

    let clean = TestRepo::new();
    clean.commit("a", "1", "one");
    let dirty = TestRepo::new();
    dirty.commit("a", "1", "one");
    dirty.write("a", "2");

    copy_tree(clean.path(), &base.path().join("clean"));
    copy_tree(dirty.path(), &base.path().join("dirty"));

    let (rows, problems) = collect(opts(base.path()));
    let names: Vec<&str> = rows.iter().map(|r| r.display_name.as_str()).collect();
    assert_eq!(rows.len(), 2, "rows = {names:?}");
    assert!(problems.is_empty());
    assert_eq!(
        rows[0].display_name, "dirty",
        "the drifted repo must sort first"
    );
}

#[test]
fn a_broken_repository_yields_an_error_row_and_does_not_abort_the_scan() {
    let base = tempfile::tempdir().unwrap();
    let good = TestRepo::new();
    good.commit("a", "1", "one");
    copy_tree(good.path(), &base.path().join("good"));

    // A directory whose `.git` is neither a valid dir nor a valid gitfile.
    let broken = base.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), "this is not a gitfile").unwrap();

    let (rows, _) = collect(opts(base.path()));
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|r| r.display_name == "broken" && r.error.is_some()));
    assert!(rows
        .iter()
        .any(|r| r.display_name == "good" && r.error.is_none()));
}

#[test]
fn stream_emits_one_status_per_repo_then_done() {
    let base = tempfile::tempdir().unwrap();
    for n in ["one", "two", "three"] {
        let r = TestRepo::new();
        r.commit("a", "1", "c");
        copy_tree(r.path(), &base.path().join(n));
    }

    let mut statuses = 0;
    let mut saw_done = false;
    for ev in stream(opts(base.path())) {
        match ev {
            Event::Status(_) => statuses += 1,
            Event::Problem(_) => {}
            Event::Done => {
                saw_done = true;
                break;
            }
        }
    }
    assert_eq!(statuses, 3);
    assert!(saw_done, "the stream must terminate with Done");
}

#[test]
fn a_missing_root_is_reported_as_a_problem() {
    let (rows, problems) = collect(ScanOptions {
        roots: vec![std::path::PathBuf::from("/definitely/not/here")],
        prune: vec![],
        threads: 2,
    });
    assert!(rows.is_empty());
    assert_eq!(problems.len(), 1);
}
