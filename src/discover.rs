use std::path::PathBuf;
use std::sync::Arc;

use ignore::{WalkBuilder, WalkState};

fn is_repo(dir: &std::path::Path) -> bool {
    // A worktree or submodule has `.git` as a file, so don't require a directory.
    dir.join(".git").exists()
}

/// `ignore::Error` has no path accessor; the path lives in a `WithPath`
/// variant that other variants wrap, so dig for it.
fn error_path(err: &ignore::Error) -> Option<&std::path::Path> {
    match err {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            error_path(err)
        }
        ignore::Error::Loop { child, .. } => Some(child),
        ignore::Error::Partial(errs) => errs.iter().find_map(|e| error_path(e)),
        _ => None,
    }
}

/// Walk `roots` in parallel, reporting each repository exactly once.
///
/// Descent stops at a repository boundary, so nested clones (vendored
/// dependencies, terragrunt module caches) never surface as their own entries.
pub fn walk(
    roots: &[PathBuf],
    prune: &[String],
    on_repo: impl Fn(Found) + Send + Sync,
    on_problem: impl Fn(Problem) + Send + Sync,
) {
    let on_repo = Arc::new(on_repo);
    let on_problem = Arc::new(on_problem);

    for root in roots {
        if !root.exists() {
            on_problem(Problem {
                path: root.clone(),
                reason: "root does not exist".to_string(),
            });
            continue;
        }

        let root_label = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());

        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false) // repos can live under dot-directories
            .follow_links(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(false) // honour .gitignore even outside a repo
            .threads(num_cpus::get());

        let pruned: Vec<String> = prune.to_vec();
        builder.filter_entry(move |e| {
            let Some(name) = e.file_name().to_str() else {
                return true;
            };
            !pruned.iter().any(|p| p == name)
        });

        let on_repo = Arc::clone(&on_repo);
        let on_problem = Arc::clone(&on_problem);

        builder.build_parallel().run(move || {
            let root_clone = root.clone();
            let root_label_clone = root_label.clone();
            let on_repo = Arc::clone(&on_repo);
            let on_problem = Arc::clone(&on_problem);

            Box::new(move |result| {
                let entry = match result {
                    Ok(e) => e,
                    Err(err) => {
                        let path = error_path(&err)
                            .map(PathBuf::from)
                            .unwrap_or_else(|| root_clone.clone());
                        on_problem(Problem {
                            path,
                            reason: err.to_string(),
                        });
                        return WalkState::Continue;
                    }
                };
                if !entry.file_type().is_some_and(|t| t.is_dir()) {
                    return WalkState::Continue;
                }
                let path = entry.path();
                if !is_repo(path) {
                    return WalkState::Continue;
                }
                let display_name = match path.strip_prefix(&root_clone) {
                    Ok(rel) if rel.as_os_str().is_empty() => root_label_clone.clone(),
                    Ok(rel) => rel.to_string_lossy().into_owned(),
                    Err(_) => path.to_string_lossy().into_owned(),
                };
                on_repo(Found {
                    path: path.to_path_buf(),
                    display_name,
                });
                WalkState::Skip
            })
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    /// Create a directory that looks like a repo to the walker.
    fn fake_repo(at: &Path) {
        fs::create_dir_all(at.join(".git")).unwrap();
    }

    fn names(root: &Path, prune: &[String]) -> Vec<String> {
        let found = Mutex::new(Vec::new());
        walk(
            &[root.to_path_buf()],
            prune,
            |f| found.lock().unwrap().push(f.display_name),
            |_| {},
        );
        let mut v = found.into_inner().unwrap();
        v.sort();
        v
    }

    fn builtin() -> Vec<String> {
        crate::config::DEFAULT_PRUNE
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn finds_repos_and_reports_paths_relative_to_the_root() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(&d.path().join("sre/alpha"));
        fake_repo(&d.path().join("platform/beta"));
        assert_eq!(
            names(d.path(), &builtin()),
            vec!["platform/beta", "sre/alpha"]
        );
    }

    #[test]
    fn does_not_descend_into_a_repo() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(&d.path().join("outer"));
        fake_repo(&d.path().join("outer/vendor_clone"));
        assert_eq!(names(d.path(), &builtin()), vec!["outer"]);
    }

    #[test]
    fn skips_pruned_directories_entirely() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(&d.path().join("keep"));
        fake_repo(&d.path().join(".terragrunt-cache/abc/module"));
        assert_eq!(names(d.path(), &builtin()), vec!["keep"]);
    }

    #[test]
    fn honours_gitignore_files() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".gitignore"), "hidden_area/\n").unwrap();
        fake_repo(&d.path().join("visible"));
        fake_repo(&d.path().join("hidden_area/invisible"));
        assert_eq!(names(d.path(), &builtin()), vec!["visible"]);
    }

    #[test]
    fn detects_dot_git_as_a_file() {
        // Worktrees and submodules use a `.git` *file*, not a directory.
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("worktree");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        assert_eq!(names(d.path(), &builtin()), vec!["worktree"]);
    }

    #[test]
    fn a_root_that_is_itself_a_repo_is_reported_by_its_own_name() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("solo");
        fake_repo(&root);
        assert_eq!(names(&root, &builtin()), vec!["solo"]);
    }

    #[test]
    fn a_missing_root_becomes_a_problem_not_a_panic() {
        let problems = Mutex::new(Vec::new());
        walk(
            &[PathBuf::from("/definitely/not/here")],
            &builtin(),
            |_| panic!("should find nothing"),
            |p| problems.lock().unwrap().push(p),
        );
        assert_eq!(problems.into_inner().unwrap().len(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn io_errors_report_the_failing_directory_not_the_root() {
        use std::os::unix::fs::PermissionsExt;

        let d = tempfile::tempdir().unwrap();
        let root = d.path();

        // Create a repo that should be found
        fake_repo(&root.join("found"));

        // Create a directory with no read permissions
        let denied = root.join("denied");
        fs::create_dir_all(&denied).unwrap();

        let found_repos = Mutex::new(Vec::new());
        let found_problems = Mutex::new(Vec::new());

        // Make denied directory unreadable
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();

        // Walk should find the repo and report a problem for the denied directory
        walk(
            &[root.to_path_buf()],
            &builtin(),
            |f| found_repos.lock().unwrap().push(f),
            |p| found_problems.lock().unwrap().push(p),
        );

        // Restore permissions so tempdir cleanup works
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();

        // Verify repo was found
        let repos = found_repos.into_inner().unwrap();
        assert!(
            repos.iter().any(|r| r.display_name == "found"),
            "should have found the accessible repo"
        );

        // Verify at least one problem was reported with path ending in "denied"
        let problems = found_problems.into_inner().unwrap();
        assert!(
            !problems.is_empty(),
            "should have reported a problem for the denied directory"
        );
        assert!(
            problems.iter().any(|p| p.path.ends_with("denied")),
            "problem should report the denied directory path, not the root: {:?}",
            problems
        );
    }
}
