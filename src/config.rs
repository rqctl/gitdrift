use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_PRUNE: &[&str] = &[
    ".terragrunt-cache",
    ".terraform",
    "node_modules",
    "target",
    ".venv",
    "vendor",
    ".cache",
    ".direnv",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub prune: Vec<String>,
    pub editor: Option<String>,
    /// Max repositories touched at once by fetch and prune.
    pub concurrency: usize,
    /// Branch names that are unremarkable. Anything else is highlighted.
    pub default_branches: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    roots: Option<Vec<String>>,
    prune: Option<Vec<String>>,
    prune_extra: Option<Vec<String>>,
    editor: Option<String>,
    concurrency: Option<usize>,
    default_branches: Option<Vec<String>>,
}

fn expand(raw: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(raw).into_owned())
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: vec![expand("~/gitlab")],
            prune: DEFAULT_PRUNE.iter().map(|s| (*s).to_string()).collect(),
            editor: None,
            concurrency: 16,
            default_branches: crate::theme::DEFAULT_BRANCHES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl Config {
    /// Path of the config file when the caller has no opinion.
    pub fn default_path() -> PathBuf {
        expand("~/.config/gitdrift/config.toml")
    }

    /// Load config, falling back to defaults. A missing file is not an error;
    /// a malformed one is.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let owned;
        let path = match path {
            Some(p) => p,
            None => {
                owned = Self::default_path();
                &owned
            }
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(Config::default());
        };
        let raw: RawConfig =
            toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

        let mut cfg = Config::default();
        if let Some(roots) = raw.roots {
            cfg.roots = roots.iter().map(|r| expand(r)).collect();
        }
        if let Some(prune) = raw.prune {
            cfg.prune = prune;
        }
        if let Some(extra) = raw.prune_extra {
            cfg.prune.extend(extra);
        }
        if let Some(editor) = raw.editor {
            cfg.editor = Some(editor);
        }
        if let Some(n) = raw.concurrency {
            cfg.concurrency = n.max(1);
        }
        if let Some(b) = raw.default_branches {
            cfg.default_branches = b;
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("config.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn default_config_uses_home_gitlab_and_builtin_prune() {
        let c = Config::default();
        assert!(c.roots[0].ends_with("gitlab"), "roots = {:?}", c.roots);
        assert!(c.prune.iter().any(|p| p == ".terragrunt-cache"));
        assert_eq!(c.concurrency, 16);
        assert_eq!(c.editor, None);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let c = Config::load(Some(Path::new("/nonexistent/gitdrift.toml"))).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn roots_are_tilde_expanded() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "roots = [\"~/somewhere\"]\n");
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.roots[0].is_absolute(), "roots = {:?}", c.roots);
        assert!(!c.roots[0].to_string_lossy().contains('~'));
    }

    #[test]
    fn prune_replaces_the_builtin_list() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "prune = [\"only_this\"]\n");
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.prune, vec!["only_this".to_string()]);
    }

    #[test]
    fn prune_extra_appends_to_the_builtin_list() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "prune_extra = [\"also_this\"]\n");
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.prune.iter().any(|p| p == ".terragrunt-cache"));
        assert!(c.prune.iter().any(|p| p == "also_this"));
    }

    #[test]
    fn default_branches_default_to_the_usual_four_and_are_overridable() {
        assert_eq!(
            Config::default().default_branches,
            ["main", "master", "trunk", "develop"]
        );
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "default_branches = [\"trunk\"]\n");
        assert_eq!(Config::load(Some(&p)).unwrap().default_branches, ["trunk"]);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "roots = not-a-list\n");
        assert!(Config::load(Some(&p)).is_err());
    }
}
