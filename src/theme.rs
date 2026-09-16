use crate::status::{Head, RepoStatus};

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// Semantic colours, taken from the terminal's own 16-colour palette so they
/// follow the user's theme and stay legible on light and dark backgrounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    BrightYellow,
    Blue,
    Green,
    Yellow,
    Cyan,
    Magenta,
    BrightMagenta,
    DimGreen,
    Dim,
    Accent,
    /// Background of the selected row. Kept out of `Facet`: it is chrome, not
    /// a repository state.
    Selection,
    /// Panel chrome (titles): Catppuccin Mocha teal, matching k9s'
    /// `frame.title.fgColor`.
    Teal,
    /// Panel chrome (borders): Catppuccin Mocha mauve, matching k9s'
    /// `frame.border.fgColor`.
    Mauve,
    /// The mark dot on a marked row: Catppuccin Mocha pink.
    Pink,
}

impl Color {
    pub fn ansi(self) -> &'static str {
        match self {
            Color::Red => "\x1b[31m",
            Color::BrightYellow => "\x1b[93m",
            Color::Blue => "\x1b[34m",
            Color::Green => "\x1b[32m",
            Color::Yellow => "\x1b[33m",
            Color::Cyan => "\x1b[36m",
            Color::Magenta => "\x1b[35m",
            Color::BrightMagenta => "\x1b[95m",
            Color::DimGreen => "\x1b[2;32m",
            Color::Dim => "\x1b[2m",
            Color::Accent => "\x1b[36m",
            Color::Selection => "\x1b[100m",
            Color::Teal => "\x1b[38;2;148;226;213m",
            Color::Mauve => "\x1b[38;2;203;166;247m",
            Color::Pink => "\x1b[38;2;245;194;231m",
        }
    }

    pub fn to_ratatui(self) -> ratatui::style::Color {
        use ratatui::style::Color as C;
        match self {
            Color::Red => C::Red,
            Color::BrightYellow => C::LightYellow,
            Color::Blue => C::Blue,
            Color::Green => C::Green,
            Color::Yellow => C::Yellow,
            Color::Cyan => C::Cyan,
            Color::Magenta => C::Magenta,
            Color::BrightMagenta => C::LightMagenta,
            Color::DimGreen => C::Green,
            Color::Dim => C::DarkGray,
            Color::Accent => C::Cyan,
            Color::Selection => C::Indexed(236),
            Color::Teal => C::Rgb(148, 226, 213),
            Color::Mauve => C::Rgb(203, 166, 247),
            Color::Pink => C::Rgb(245, 194, 231),
        }
    }
}

/// Panel title style: bold teal, Catppuccin Mocha's k9s-style chrome.
pub fn title_style() -> ratatui::style::Style {
    ratatui::style::Style::default()
        .fg(Color::Teal.to_ratatui())
        .add_modifier(ratatui::style::Modifier::BOLD)
}

/// Panel border style: Catppuccin Mocha mauve, matching `title_style`'s chrome.
pub fn border_style() -> ratatui::style::Style {
    ratatui::style::Style::default().fg(Color::Mauve.to_ratatui())
}

/// One aspect of a repository's state. Each has exactly one colour and one
/// glyph, used identically in the TUI and in `--plain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Error,
    Conflict,
    Diverged,
    Ahead,
    Behind,
    Staged,
    Unstaged,
    Untracked,
    Stashed,
    Detached,
    NoUpstream,
    Clean,
    Added,
    Removed,
}

impl Facet {
    pub const ALL: [Facet; 14] = [
        Facet::Error,
        Facet::Conflict,
        Facet::Diverged,
        Facet::Ahead,
        Facet::Behind,
        Facet::Staged,
        Facet::Unstaged,
        Facet::Untracked,
        Facet::Stashed,
        Facet::Detached,
        Facet::NoUpstream,
        Facet::Clean,
        Facet::Added,
        Facet::Removed,
    ];

    pub fn glyph(self) -> char {
        match self {
            Facet::Error => '!',
            Facet::Conflict => '✖',
            Facet::Diverged => '⇅',
            Facet::Ahead => '↑',
            Facet::Behind => '↓',
            Facet::Staged => '✚',
            Facet::Unstaged => '●',
            Facet::Untracked => '?',
            Facet::Stashed => '⚑',
            Facet::Detached => '⌀',
            Facet::NoUpstream => '⊘',
            Facet::Clean => '✔',
            Facet::Added => '+',
            Facet::Removed => '-',
        }
    }

    pub fn color(self) -> Color {
        match self {
            Facet::Error => Color::Red,
            Facet::Conflict => Color::Red,
            Facet::Diverged => Color::Red,
            Facet::Ahead => Color::BrightYellow,
            Facet::Behind => Color::Blue,
            Facet::Staged => Color::Green,
            Facet::Unstaged => Color::Yellow,
            Facet::Untracked => Color::Cyan,
            Facet::Stashed => Color::Magenta,
            Facet::Detached => Color::BrightMagenta,
            Facet::NoUpstream => Color::BrightMagenta,
            Facet::Clean => Color::DimGreen,
            Facet::Added => Color::Green,
            Facet::Removed => Color::Red,
        }
    }

    pub fn bold(self) -> bool {
        matches!(self, Facet::Error | Facet::Conflict)
    }
}

/// Branch names that mean "nothing unusual here".
pub const DEFAULT_BRANCHES: &[&str] = &["main", "master", "trunk", "develop"];

/// Style for a repository's HEAD label.
///
/// A branch that isn't one of the usual defaults is worth noticing, so it
/// renders at full weight while the ordinary ones stay dim. Deliberately a
/// weight change, not a hue: every colour in the palette already means a
/// status, and a branch name is not a status.
pub fn branch_style(s: &RepoStatus, default_branches: &[String]) -> ratatui::style::Style {
    use ratatui::style::{Modifier, Style};
    let ordinary = match &s.head {
        Head::Branch(name) => default_branches.iter().any(|d| d == name),
        // Detached and unborn already carry their own signal in the glyph column.
        _ => true,
    };
    if ordinary {
        Style::default().fg(Color::Dim.to_ratatui())
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

/// The most severe facet present, used to colour the repository name.
///
/// This order is deliberately not identical to `drift::score`: a detached HEAD
/// ranks higher here because it is the thing you most need to *notice* about a
/// row, while contributing less to where the row sorts.
pub fn worst(s: &RepoStatus) -> Facet {
    if s.error.is_some() {
        return Facet::Error;
    }
    if s.conflicted > 0 {
        return Facet::Conflict;
    }
    if s.ahead > 0 && s.behind > 0 {
        return Facet::Diverged;
    }
    if s.ahead > 0 {
        return Facet::Ahead;
    }
    if matches!(s.head, Head::Detached(_)) {
        return Facet::Detached;
    }
    if s.staged > 0 || s.unstaged > 0 {
        return Facet::Unstaged;
    }
    if s.behind > 0 {
        return Facet::Behind;
    }
    if s.upstream.is_none() && !matches!(s.head, Head::Unborn) {
        return Facet::NoUpstream;
    }
    if s.untracked > 0 {
        return Facet::Untracked;
    }
    if s.stash_count > 0 {
        return Facet::Stashed;
    }
    Facet::Clean
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

pub fn should_colorize(mode: ColorMode, stdout_is_tty: bool, no_color_env: bool) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => stdout_is_tty && !no_color_env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn row() -> RepoStatus {
        RepoStatus {
            path: PathBuf::from("/tmp/x"),
            display_name: "x".into(),
            head: Head::Branch("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            stash_count: 0,
            last_commit_time: Some(1),
            fetch_age: None,
            error: None,
        }
    }

    #[test]
    fn every_facet_has_a_distinct_glyph() {
        let mut glyphs: Vec<char> = Facet::ALL.iter().map(|f| f.glyph()).collect();
        glyphs.sort_unstable();
        let before = glyphs.len();
        glyphs.dedup();
        assert_eq!(
            glyphs.len(),
            before,
            "glyphs must be unique: colour is never the only signal"
        );
    }

    #[test]
    fn worst_facet_reflects_the_most_severe_state() {
        let mut r = row();
        assert_eq!(worst(&r), Facet::Clean);

        r.stash_count = 1;
        assert_eq!(worst(&r), Facet::Stashed);

        r.untracked = 1;
        assert_eq!(worst(&r), Facet::Untracked);

        r.behind = 1;
        assert_eq!(worst(&r), Facet::Behind);

        r.unstaged = 1;
        assert_eq!(worst(&r), Facet::Unstaged);

        r.ahead = 1;
        assert_eq!(worst(&r), Facet::Diverged, "ahead + behind is divergence");

        r.behind = 0;
        assert_eq!(worst(&r), Facet::Ahead);

        r.conflicted = 1;
        assert_eq!(worst(&r), Facet::Conflict);

        r.error = Some("boom".into());
        assert_eq!(worst(&r), Facet::Error);
    }

    #[test]
    fn no_upstream_is_surfaced_on_an_otherwise_quiet_repo() {
        let mut r = row();
        r.upstream = None;
        assert_eq!(worst(&r), Facet::NoUpstream);
    }

    #[test]
    fn an_unborn_repo_reads_as_clean_not_as_missing_an_upstream() {
        let mut r = row();
        r.head = Head::Unborn;
        r.upstream = None;
        assert_eq!(worst(&r), Facet::Clean);
    }

    #[test]
    fn color_mode_never_and_always_win_over_the_environment() {
        assert!(!should_colorize(ColorMode::Never, true, false));
        assert!(should_colorize(ColorMode::Always, false, true));
    }

    #[test]
    fn auto_respects_tty_and_no_color() {
        assert!(should_colorize(ColorMode::Auto, true, false));
        assert!(
            !should_colorize(ColorMode::Auto, false, false),
            "piped output"
        );
        assert!(
            !should_colorize(ColorMode::Auto, true, true),
            "NO_COLOR is set"
        );
    }

    #[test]
    fn ansi_codes_are_escape_sequences_and_reset_is_available() {
        assert!(Color::Red.ansi().starts_with('\x1b'));
        assert_eq!(RESET, "\x1b[0m");
    }
}
