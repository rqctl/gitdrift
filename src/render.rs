use crate::status::{Head, RepoStatus};
use crate::theme::{self, Facet, BOLD, RESET};

/// The counter column: one cell per present state, in a fixed order so the
/// eye learns the positions.
pub fn status_cells(s: &RepoStatus) -> Vec<(Facet, String)> {
    if s.error.is_some() {
        return vec![(Facet::Error, Facet::Error.glyph().to_string())];
    }
    let mut cells = Vec::new();
    let mut push = |f: Facet, n: u32| {
        if n > 0 {
            cells.push((f, format!("{}{}", f.glyph(), n)));
        }
    };
    push(Facet::Conflict, s.conflicted);
    push(Facet::Ahead, s.ahead);
    push(Facet::Behind, s.behind);
    push(Facet::Staged, s.staged);
    push(Facet::Unstaged, s.unstaged);
    push(Facet::Untracked, s.untracked);
    push(Facet::Stashed, s.stash_count);
    if matches!(s.head, Head::Detached(_)) {
        cells.push((Facet::Detached, Facet::Detached.glyph().to_string()));
    }
    if s.upstream.is_none() && !matches!(s.head, Head::Unborn) {
        cells.push((Facet::NoUpstream, Facet::NoUpstream.glyph().to_string()));
    }
    if cells.is_empty() {
        cells.push((Facet::Clean, Facet::Clean.glyph().to_string()));
    }
    cells
}

pub fn head_label(s: &RepoStatus) -> String {
    match &s.head {
        Head::Branch(b) => b.clone(),
        Head::Detached(sha) => format!("({sha})"),
        Head::Unborn => "(unborn)".to_string(),
    }
}

/// Shorten to `width`, keeping the tail: the repository name matters more
/// than the namespace it sits in.
pub fn elide(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    std::iter::once('…')
        .chain(s.chars().skip(len - (width - 1)))
        .collect()
}

/// Shorten `a/b/c/name` to `width` the way p10k shortens a prompt: leading
/// segments stay, the middle collapses to `…`, and the final segment — the
/// repository name, the part being looked for — is never truncated.
pub fn shorten_path(path: &str, width: usize) -> String {
    if path.chars().count() <= width {
        return path.to_string();
    }
    let Some((head, last)) = path.rsplit_once('/') else {
        return elide(path, width);
    };
    // Room for the name plus the "…/" standing in for what was dropped. If
    // the name alone will not fit, it still wins: a truncated name is useless.
    let Some(budget) = width.checked_sub(last.chars().count() + 2) else {
        return format!("…/{last}");
    };
    let mut kept = String::new();
    for seg in head.split('/') {
        if kept.chars().count() + seg.chars().count() + 1 > budget {
            break;
        }
        kept.push_str(seg);
        kept.push('/');
    }
    format!("{kept}…/{last}")
}

/// The width the status column occupies once rendered, trailing gap included.
pub fn status_width(s: &RepoStatus) -> usize {
    status_cells(s)
        .iter()
        .map(|(_, text)| text.chars().count() + 1)
        .sum()
}

fn paint(text: &str, facet: Facet, colorize: bool) -> String {
    if !colorize {
        return text.to_string();
    }
    let bold = if facet.bold() { BOLD } else { "" };
    format!("{}{}{}{}", bold, facet.color().ansi(), text, RESET)
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// An aligned table. Column widths adapt to the content.
pub fn plain(rows: &[RepoStatus], colorize: bool) -> String {
    let name_w = rows
        .iter()
        .map(|r| r.display_name.chars().count())
        .max()
        .unwrap_or(4);
    let head_w = rows
        .iter()
        .map(|r| head_label(r).chars().count())
        .max()
        .unwrap_or(6)
        .min(30);

    let mut out = String::new();
    for r in rows {
        out.push_str(&paint(
            &pad(&r.display_name, name_w),
            theme::worst(r),
            colorize,
        ));
        out.push_str("  ");
        out.push_str(&pad(&head_label(r), head_w));
        out.push_str("  ");
        let cells: Vec<String> = status_cells(r)
            .into_iter()
            .map(|(f, text)| paint(&text, f, colorize))
            .collect();
        out.push_str(&cells.join(" "));
        if let Some(e) = &r.error {
            out.push_str("  ");
            out.push_str(&paint(e, Facet::Error, colorize));
        }
        out.push('\n');
    }
    out
}

pub fn json(rows: &[RepoStatus]) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(rows)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn shorten_path_collapses_the_middle_and_keeps_the_repository_name() {
        use super::shorten_path;
        let p = "idp/developer-control-plane/workflow-automation-n8n/datatables";
        assert_eq!(shorten_path(p, 100), p, "it fits, leave it alone");
        assert_eq!(
            shorten_path(p, 45),
            "idp/developer-control-plane/…/datatables",
            "the middle goes first"
        );
        assert_eq!(
            shorten_path(p, 20),
            "idp/…/datatables",
            "then the head, segment by segment"
        );
        assert_eq!(
            shorten_path(p, 14),
            "…/datatables",
            "the repository name is the last thing standing"
        );
    }

    #[test]
    fn shorten_path_keeps_the_repository_name_even_when_it_alone_overflows() {
        use super::shorten_path;
        assert_eq!(
            super::shorten_path("a/b/an-unusually-long-repository-name", 10),
            "…/an-unusually-long-repository-name",
            "a truncated name would be useless; overflow instead"
        );
        assert_eq!(
            shorten_path("no-slashes-at-all", 8),
            "…-at-all",
            "a path with no segments has nothing to collapse; elide it"
        );
    }

    #[test]
    fn elide_keeps_the_tail_of_an_overlong_name() {
        use super::elide;
        assert_eq!(elide("short", 20), "short");
        assert_eq!(elide("abcdef", 6), "abcdef");
        assert_eq!(elide("abcdef", 4), "…def");
        assert_eq!(elide("abcdef", 1), "…");
        assert_eq!(elide("abcdef", 0), "");
    }

    use super::*;
    use std::path::PathBuf;

    fn row(name: &str) -> RepoStatus {
        RepoStatus {
            path: PathBuf::from("/tmp").join(name),
            display_name: name.to_string(),
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
    fn a_clean_repo_shows_only_the_clean_glyph() {
        let cells = status_cells(&row("x"));
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].0, Facet::Clean);
        assert_eq!(cells[0].1, "✔");
    }

    #[test]
    fn a_repo_with_nowhere_to_push_is_not_reported_as_clean() {
        let mut r = row("x");
        r.upstream = None;
        let glyphs: Vec<String> = status_cells(&r).into_iter().map(|(_, t)| t).collect();
        assert_eq!(glyphs, ["⊘"], "no upstream is a state, not cleanliness");

        r.head = Head::Detached("1a2b3c4".into());
        let glyphs: Vec<String> = status_cells(&r).into_iter().map(|(_, t)| t).collect();
        assert_eq!(glyphs, ["⌀", "⊘"]);

        r.head = Head::Unborn;
        let glyphs: Vec<String> = status_cells(&r).into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            glyphs,
            ["✔"],
            "a repo with no commits has no upstream to lack"
        );
    }

    #[test]
    fn cells_carry_counts_next_to_their_glyph_in_a_fixed_order() {
        let mut r = row("x");
        r.ahead = 2;
        r.behind = 3;
        r.staged = 1;
        r.unstaged = 4;
        r.untracked = 5;
        r.stash_count = 6;
        let rendered: Vec<String> = status_cells(&r).into_iter().map(|(_, s)| s).collect();
        assert_eq!(rendered, vec!["↑2", "↓3", "✚1", "●4", "?5", "⚑6"]);
    }

    #[test]
    fn an_error_row_shows_only_the_error_glyph() {
        let mut r = row("x");
        r.unstaged = 9;
        r.error = Some("bad".into());
        let cells = status_cells(&r);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].0, Facet::Error);
    }

    #[test]
    fn plain_output_without_colour_contains_no_escape_sequences() {
        let mut r = row("sre/alpha");
        r.ahead = 1;
        let out = plain(&[r], false);
        assert!(!out.contains('\x1b'), "escapes leaked: {out:?}");
        assert!(out.contains("sre/alpha"));
        assert!(out.contains("↑1"));
    }

    #[test]
    fn plain_output_with_colour_always_resets_what_it_opens() {
        let mut r = row("x");
        r.ahead = 1;
        let out = plain(&[r], true);
        assert!(out.contains('\x1b'));
        assert!(out.ends_with('\n'));
        let starts = out.matches("\x1b[").count();
        let resets = out.matches(RESET).count();
        assert!(
            resets > 0 && starts > resets,
            "starts={starts} resets={resets}"
        );
    }

    #[test]
    fn plain_renders_head_state_readably() {
        let mut detached = row("d");
        detached.head = Head::Detached("abc1234".into());
        assert!(plain(&[detached], false).contains("abc1234"));

        let mut unborn = row("u");
        unborn.head = Head::Unborn;
        assert!(plain(&[unborn], false).contains("(unborn)"));
    }

    #[test]
    fn plain_shows_the_error_reason() {
        let mut r = row("broken");
        r.error = Some("not a repository".into());
        let out = plain(&[r], false);
        assert!(out.contains("not a repository"), "{out}");
    }

    #[test]
    fn plain_on_an_empty_slice_is_empty_not_a_panic() {
        assert_eq!(plain(&[], false), "");
    }

    #[test]
    fn json_round_trips_the_important_fields() {
        let mut r = row("sre/alpha");
        r.ahead = 2;
        r.stash_count = 1;
        let text = json(&[r]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed[0]["display_name"], "sre/alpha");
        assert_eq!(parsed[0]["ahead"], 2);
        assert_eq!(parsed[0]["stash_count"], 1);
        assert_eq!(parsed[0]["head"]["kind"], "branch");
        assert_eq!(parsed[0]["head"]["value"], "main");
    }

    #[test]
    fn json_is_an_array_even_when_empty() {
        assert_eq!(json(&[]).unwrap().trim(), "[]");
    }
}
