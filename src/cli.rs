use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::config::Config;
use crate::drift;
use crate::render;
use crate::scan::{self, ScanOptions};
use crate::theme::{self, ColorMode};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ColorArg {
    Auto,
    Always,
    Never,
}

impl From<ColorArg> for ColorMode {
    fn from(a: ColorArg) -> Self {
        match a {
            ColorArg::Auto => ColorMode::Auto,
            ColorArg::Always => ColorMode::Always,
            ColorArg::Never => ColorMode::Never,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "gitdrift",
    version,
    about = "Find git repositories that have drifted"
)]
pub struct Args {
    /// Directory to scan. Defaults to the roots in the config file.
    pub path: Option<PathBuf>,

    /// Print an aligned table and exit instead of opening the TUI.
    #[arg(long, conflicts_with = "json")]
    pub plain: bool,

    /// Print JSON and exit instead of opening the TUI.
    #[arg(long)]
    pub json: bool,

    /// Hide repositories that are clean and in sync.
    #[arg(long)]
    pub drifted_only: bool,

    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorArg,

    /// Alternate config file.
    #[arg(long)]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    let cfg = Config::load(args.config.as_deref())?;
    let mut opts = ScanOptions::from_config(&cfg);
    if let Some(p) = &args.path {
        opts.roots = vec![p.clone()];
    }

    if args.plain || args.json {
        let (mut rows, problems) = scan::collect(opts);
        if args.drifted_only {
            rows.retain(drift::is_drifted);
        }

        let mut stdout = std::io::stdout().lock();
        if args.json {
            writeln!(stdout, "{}", render::json(&rows)?)?;
        } else {
            let colorize = theme::should_colorize(
                args.color.into(),
                std::io::stdout().is_terminal(),
                std::env::var_os("NO_COLOR").is_some(),
            );
            write!(stdout, "{}", render::plain(&rows, colorize))?;
        }

        let mut stderr = std::io::stderr().lock();
        for p in &problems {
            writeln!(stderr, "gitdrift: {}: {}", p.path.display(), p.reason)?;
        }
        // Every root unreadable is a failure; individual bad repos are not.
        return Ok(if rows.is_empty() && !problems.is_empty() {
            1
        } else {
            0
        });
    }

    crate::ui::app::run(opts, &cfg, args.drifted_only)?;
    Ok(0)
}
