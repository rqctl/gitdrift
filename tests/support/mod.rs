#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// A real git repository in a tempdir, built by shelling out to `git`.
///
/// Fixtures use the real binary on purpose: they are the oracle we check
/// gix's answers against, so they must not share gix's bugs.
pub struct TestRepo {
    pub dir: tempfile::TempDir,
    /// Scratch space *outside* the worktree, for upstream clones. Must not
    /// live under `dir` (it would show up as an untracked file) nor under the
    /// shared temp root (parallel tests would collide on the same path).
    pub aux: tempfile::TempDir,
}

impl TestRepo {
    pub fn new() -> Self {
        let r = TestRepo {
            dir: tempfile::tempdir().unwrap(),
            aux: tempfile::tempdir().unwrap(),
        };
        r.git(&["init", "-q", "-b", "main"]);
        r.git(&["config", "user.email", "test@example.com"]);
        r.git(&["config", "user.name", "Test"]);
        r.git(&["config", "commit.gpgsign", "false"]);
        r
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(self.path(), args)
    }

    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn write(&self, rel: &str, body: &str) {
        let p = self.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    pub fn commit(&self, rel: &str, body: &str, message: &str) {
        self.write(rel, body);
        self.git(&["add", rel]);
        self.git(&["commit", "-qm", message]);
    }

    /// Create a bare upstream in the aux dir and track it. Returns its path.
    pub fn with_upstream(&self) -> PathBuf {
        let up = self.aux.path().join("upstream.git");
        self.git(&["clone", "-q", "--bare", ".", up.to_str().unwrap()]);
        self.git(&["remote", "add", "origin", up.to_str().unwrap()]);
        self.git(&["fetch", "-q", "origin"]);
        self.git(&["branch", "--set-upstream-to=origin/main", "main"]);
        up
    }

    /// Push a commit into `up` through a throwaway second clone, so this repo
    /// becomes genuinely behind. Returns the tempdir holding the clone; keep it
    /// alive for the duration of the test.
    pub fn advance_upstream(&self, up: &Path, file: &str) -> tempfile::TempDir {
        let other = tempfile::tempdir().unwrap();
        let clone = other.path().join("clone");
        self.git_in(
            other.path(),
            &["clone", "-q", up.to_str().unwrap(), clone.to_str().unwrap()],
        );
        self.git_in(&clone, &["config", "user.email", "t@e.com"]);
        self.git_in(&clone, &["config", "user.name", "T"]);
        std::fs::write(clone.join(file), "upstream").unwrap();
        self.git_in(&clone, &["add", file]);
        self.git_in(&clone, &["commit", "-qm", "upstream commit"]);
        self.git_in(&clone, &["push", "-q", "origin", "main"]);
        other
    }
}

/// Recursive copy, so fixtures built in their own tempdirs can be gathered
/// under one scan root.
pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            let dst = to.join(p.strip_prefix(from).unwrap());
            if p.is_dir() {
                std::fs::create_dir_all(&dst).unwrap();
                stack.push(p);
            } else {
                std::fs::copy(&p, &dst).unwrap();
            }
        }
    }
}
