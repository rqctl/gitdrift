mod support;

use assert_cmd::Command;
use predicates::str::contains;
use support::{copy_tree, TestRepo};

fn dirty_root() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let r = TestRepo::new();
    r.commit("a", "1", "one");
    r.write("a", "2");
    copy_tree(r.path(), &base.path().join("dirty"));
    base
}

#[test]
fn plain_mode_lists_the_repository() {
    let base = dirty_root();
    Command::cargo_bin("gitdrift")
        .unwrap()
        .args(["--plain", base.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("dirty"));
}

#[test]
fn plain_mode_piped_emits_no_escape_sequences() {
    let base = dirty_root();
    let out = Command::cargo_bin("gitdrift")
        .unwrap()
        .args(["--plain", base.path().to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains('\x1b'), "escapes in piped output: {text:?}");
}

#[test]
fn color_always_forces_escape_sequences_even_when_piped() {
    let base = dirty_root();
    let out = Command::cargo_bin("gitdrift")
        .unwrap()
        .args([
            "--plain",
            "--color",
            "always",
            base.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains('\x1b'));
}

#[test]
fn no_color_suppresses_colour_under_auto() {
    let base = dirty_root();
    let out = Command::cargo_bin("gitdrift")
        .unwrap()
        .env("NO_COLOR", "1")
        .args(["--plain", base.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\x1b'));
}

#[test]
fn json_mode_emits_parseable_json() {
    let base = dirty_root();
    let out = Command::cargo_bin("gitdrift")
        .unwrap()
        .args(["--json", base.path().to_str().unwrap()])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v.is_array());
    assert_eq!(v[0]["display_name"], "dirty");
}

#[test]
fn drifted_only_hides_clean_repositories() {
    let base = tempfile::tempdir().unwrap();
    let clean = TestRepo::new();
    clean.commit("a", "1", "one");
    clean.with_upstream(); // in sync with upstream, or "no upstream" itself counts as drift
    copy_tree(clean.path(), &base.path().join("clean_one"));

    let out = Command::cargo_bin("gitdrift")
        .unwrap()
        .args(["--json", "--drifted-only", base.path().to_str().unwrap()])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn a_missing_root_exits_non_zero_with_a_message_on_stderr() {
    Command::cargo_bin("gitdrift")
        .unwrap()
        .args(["--plain", "/definitely/not/here"])
        .assert()
        .failure()
        .stderr(contains("does not exist"));
}

#[test]
fn plain_and_json_together_is_rejected() {
    Command::cargo_bin("gitdrift")
        .unwrap()
        .args(["--plain", "--json"])
        .assert()
        .failure();
}
