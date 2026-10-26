use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::detail::{CommitInfo, FileDelta, RepoDetail};
use crate::drift;
use crate::render::{
    branches_label, elide, head_label, shorten_path, status_cells, Cols, BRANCHES_HEADER,
    BRANCH_HEADER, CURSOR_W, MARK_W,
};
use crate::status::RepoStatus;
use crate::theme::{self, Facet};
use crate::ui::state::{App, Job, Pane};

/// 2 content rows (tally with activity, hints) plus the block's own border.
const HEADER_HEIGHT: u16 = 4;

fn facet_style(f: Facet) -> Style {
    let mut s = Style::default().fg(f.color().to_ratatui());
    if f.bold() {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

fn dim() -> Style {
    Style::default().fg(theme::Color::Dim.to_ratatui())
}

fn accent() -> Style {
    Style::default().fg(theme::Color::Accent.to_ratatui())
}

/// A `+++---` bar scaled so the widest file in the set fills `width`.
fn stat_bar(d: &FileDelta, max: u32, width: usize) -> Vec<Span<'static>> {
    let total = d.added + d.removed;
    if total == 0 || max == 0 {
        return Vec::new();
    }
    let cells = ((total as usize * width) / max as usize).max(1);
    let plus = (d.added as usize * cells) / total as usize;
    let minus = cells - plus;
    let mut spans = Vec::new();
    if plus > 0 {
        spans.push(Span::styled("+".repeat(plus), facet_style(Facet::Added)));
    }
    if minus > 0 {
        spans.push(Span::styled("-".repeat(minus), facet_style(Facet::Removed)));
    }
    spans
}

/// The repo name, with its parent path dimmed so the eye lands on the name.
/// Returns the rendered width alongside, which elision makes unpredictable.
fn name_spans_width(s: &RepoStatus, width: usize) -> (Vec<Span<'static>>, usize) {
    let style = facet_style(theme::worst(s)).add_modifier(Modifier::BOLD);
    let text = shorten_path(&s.display_name, width);
    let used = text.chars().count();
    let spans = match text.rsplit_once('/') {
        Some((parent, base)) => vec![
            Span::styled(format!("{parent}/"), dim()),
            Span::styled(base.to_string(), style),
        ],
        None => vec![Span::styled(text, style)],
    };
    (spans, used)
}

fn name_spans(s: &RepoStatus) -> Vec<Span<'static>> {
    name_spans_width(s, usize::MAX).0
}

fn section_span(title: &str) -> Span<'static> {
    Span::styled(
        title.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    )
}

fn section(title: &str) -> Line<'static> {
    Line::from(section_span(title))
}

fn pad_to(spans: &mut Vec<Span<'static>>, used: usize, width: usize) {
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
}

const NAME_HEADER: &str = "NAME";
/// Widest the incoming +/- bar is allowed to get.
const BAR_MAX: usize = 24;
const DRIFT_HEADER: &str = "DRIFT";

/// A gutter bar, not an inverted row: reversing a line would destroy the
/// status colours. The glyph carries the cursor where the band cannot.
fn cursor(selected: bool) -> Span<'static> {
    if selected {
        Span::styled("\u{258c}", accent())
    } else {
        Span::raw(" ")
    }
}

fn selection_bg() -> ratatui::style::Color {
    theme::Color::Selection.to_ratatui()
}

/// Lays the selection band under a row. Dim spans are lifted to the default
/// colour first: dim grey on the band is unreadable.
fn banded(line: Line<'static>, width: usize) -> Line<'static> {
    let dim_fg = theme::Color::Dim.to_ratatui();
    let pad = width.saturating_sub(line.width());
    let mut spans = line
        .spans
        .into_iter()
        .map(|mut span| {
            if span.style.fg == Some(dim_fg) {
                span.style.fg = None;
            }
            span.style = span.style.bg(selection_bg()).add_modifier(Modifier::BOLD);
            span
        })
        .collect::<Vec<_>>();
    if pad > 0 {
        spans.push(Span::styled(
            " ".repeat(pad),
            Style::default().bg(selection_bg()),
        ));
    }
    Line::from(spans)
}

fn row_line(
    s: &RepoStatus,
    marked: bool,
    selected: bool,
    cols: Cols,
    default_branches: &[String],
) -> Line<'static> {
    let mut spans = vec![
        cursor(selected),
        Span::styled(
            if marked { "◆ " } else { "  " },
            Style::default().fg(theme::Color::Pink.to_ratatui()),
        ),
    ];
    let (name, used) = name_spans_width(s, cols.name);
    spans.extend(name);
    pad_to(&mut spans, used, cols.name);
    spans.push(Span::raw("  "));

    let head = elide(&head_label(s), cols.head);
    let head_len = head.chars().count();
    spans.push(Span::styled(head, theme::branch_style(s, default_branches)));
    pad_to(&mut spans, head_len, cols.head);
    spans.push(Span::raw("  "));

    let branches = branches_label(s);
    let branches_len = branches.chars().count();
    spans.push(Span::styled(branches, dim()));
    pad_to(&mut spans, branches_len, cols.branches);
    spans.push(Span::raw("  "));

    for (facet, text) in status_cells(s) {
        spans.push(Span::styled(text, facet_style(facet)));
        spans.push(Span::raw(" "));
    }
    if let Some(e) = &s.error {
        spans.push(Span::styled(e.clone(), facet_style(Facet::Error)));
    }
    let line = Line::from(spans);
    if selected {
        banded(line, cols.total)
    } else {
        line
    }
}

/// What the event loop needs back from a frame: wheel routing and scroll bounds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rendered {
    pub detail_area: Option<Rect>,
    pub pane_max_scroll: u16,
}

/// Whether the side pane, at `total_width`, would show every visible
/// repository's name whole rather than middle-eliding it.
fn side_pane_fits(app: &App, total_width: u16) -> bool {
    let visible = app.visible();
    if visible.is_empty() {
        return true;
    }
    let candidate = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(Rect::new(0, 0, total_width, 1))[0];
    // Two border columns plus the block's own horizontal padding.
    let inner = candidate.width.saturating_sub(4) as usize;
    let cols = Cols::measure(&visible, inner);
    // `shorten_path` collapses the namespace first; only the repository
    // name itself — the last segment — ever gets middle-elided.
    visible.iter().all(|r| {
        let name = r
            .display_name
            .rsplit_once('/')
            .map_or(r.display_name.as_str(), |(_, n)| n);
        name.chars().count() <= cols.name
    })
}

pub fn draw(
    frame: &mut Frame,
    app: &App,
    detail: Option<&RepoDetail>,
    list_state: &mut ListState,
) -> Rendered {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(HEADER_HEIGHT), Constraint::Min(1)])
        .split(area);

    draw_header(frame, app, rows[0]);

    let wide_fits = side_pane_fits(app, area.width);
    let detail_open = app.pane() == Pane::List && (app.zoomed() || app.detail_open(wide_fits));
    let show_detail_side = detail_open && wide_fits && !app.zoomed();
    let show_detail_full = detail_open && !show_detail_side;
    let body = if show_detail_side {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(rows[1])
    };

    let mut max_scroll = 0;
    match app.pane() {
        Pane::Help => max_scroll = draw_help(frame, app, body[0]),
        Pane::Confirm => draw_confirm(frame, app, body[0]),
        Pane::Problems => max_scroll = draw_problems(frame, app, body[0]),
        Pane::Namespaces => {
            draw_list(frame, app, body[0], list_state);
            draw_namespaces(frame, app, body[0]);
        }
        Pane::Branches => {
            draw_list(frame, app, body[0], list_state);
            draw_branches(frame, app, body[0]);
        }
        Pane::Stashes => {
            draw_list(frame, app, body[0], list_state);
            draw_stashes(frame, app, body[0]);
        }
        Pane::Sort => {
            draw_list(frame, app, body[0], list_state);
            draw_sort(frame, app, body[0]);
        }
        Pane::List => {
            if show_detail_full {
                max_scroll = draw_detail(frame, app, detail, body[0]);
            } else {
                draw_list(frame, app, body[0], list_state);
                if show_detail_side {
                    max_scroll = draw_detail(frame, app, detail, body[1]);
                }
            }
        }
    }
    if let Some(job) = app.modal_job() {
        frame.buffer_mut().set_style(
            area,
            Style::default()
                .fg(theme::Color::Dim.to_ratatui())
                .add_modifier(Modifier::DIM)
                .remove_modifier(Modifier::BOLD),
        );
        draw_progress(frame, job, area);
    }
    Rendered {
        detail_area: if show_detail_side {
            Some(body[1])
        } else if show_detail_full {
            Some(body[0])
        } else {
            None
        },
        pane_max_scroll: max_scroll,
    }
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, state: &mut ListState) {
    let visible = app.visible();
    let count = if app.is_scanning() {
        format!("Scanning… {}", visible.len())
    } else {
        format!("{} repositories", visible.len())
    };
    let scope = app.group().unwrap_or("all");
    let stale = if app.order_stale() {
        " ↻ press o to re-sort"
    } else {
        ""
    };
    let mut flags = String::new();
    if !app.filter().is_empty() {
        flags.push_str(&format!(" · /{}", clip(app.filter(), 20)));
    }
    if app.drifted_only() {
        flags.push_str(" · drifted only");
    }
    let title = format!(
        " {count} · {scope} · by {}{flags}{stale} ",
        app.sort_mode().label()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .padding(Padding::horizontal(1))
        .title(title)
        .title_style(theme::title_style());

    if visible.is_empty() {
        let msg = if app.is_scanning() {
            "Scanning…"
        } else {
            "No repositories matched."
        };
        frame.render_widget(Paragraph::new(msg).block(block).style(dim()), area);
        state.select(None);
        return;
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let cols = Cols::measure(&visible, inner.width as usize);
    frame.render_widget(Paragraph::new(header_line(cols)), rows[0]);

    let selected = app.selected_index();
    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(i, r)| {
            ListItem::new(row_line(
                r,
                app.is_marked(&r.path),
                i == selected,
                cols,
                app.default_branches(),
            ))
        })
        .collect();
    // The band is painted into the row itself so it can lift the dim spans;
    // the widget's own highlight would only fight it.
    let list = List::new(items);
    state.select(Some(app.selected_index()));
    frame.render_stateful_widget(list, rows[1], state);
}

/// Column labels above the list, aligned with `row_line`'s own columns.
fn header_line(cols: Cols) -> Line<'static> {
    let style = Style::default()
        .fg(theme::Color::Yellow.to_ratatui())
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ".repeat(CURSOR_W + MARK_W))];

    let mut push = |label: &'static str, width: usize| {
        let used = label.chars().count();
        spans.push(Span::styled(label, style));
        pad_to(&mut spans, used, width);
        spans.push(Span::raw("  "));
    };
    push(NAME_HEADER, cols.name);
    push(BRANCH_HEADER, cols.head);
    push(BRANCHES_HEADER, cols.branches);
    spans.push(Span::styled(DRIFT_HEADER, style));

    Line::from(spans)
}

/// Renders a paragraph that may be taller than its pane, and reports how far
/// it can still scroll. The title carries ▴▾ for the directions still open.
fn scrollable(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    scroll: u16,
) -> u16 {
    // Two border columns plus the block's padding on each side.
    let inner_w = area.width.saturating_sub(4);
    let inner_h = area.height.saturating_sub(2);
    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max = (body.line_count(inner_w) as u16).saturating_sub(inner_h);
    let scroll = scroll.min(max);

    let arrows = match (scroll > 0, scroll < max) {
        (true, true) => "▴▾ ",
        (true, false) => "▴ ",
        (false, true) => "▾ ",
        (false, false) => "",
    };
    frame.render_widget(
        body.block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_style())
                .padding(Padding::horizontal(1))
                .title(format!(" {title} {arrows}"))
                .title_style(theme::title_style()),
        )
        .scroll((scroll, 0)),
        area,
    );
    max
}

/// Keys in bold accent, their labels dim, so the action reads as a key to press
/// and not as more detail text.
fn shortcuts(items: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for (i, (key, label)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", dim()));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            accent().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), dim()));
    }
    Line::from(spans)
}

fn subsection(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {title}"),
        dim().add_modifier(Modifier::BOLD),
    ))
}

fn warn() -> Style {
    Style::default()
        .fg(theme::Color::Yellow.to_ratatui())
        .add_modifier(Modifier::BOLD)
}

/// Seconds elapsed as at most four characters: `now`, `5m`, `2h`, `364d`, `2y`.
/// There is no month unit: days run up to a year, then years (365 days each,
/// rounded down).
fn age_label(secs: i64) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const YEAR: i64 = 365 * DAY;
    match secs {
        s if s < MIN => "now".to_string(),
        s if s < HOUR => format!("{}m", s / MIN),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < YEAR => format!("{}d", s / DAY),
        s => format!("{}y", (s / YEAR).min(99)),
    }
}

/// `indent`, age, short id, summary and author on one row of `width` columns,
/// the author flush right. The author goes first when the summary would be
/// left too short.
fn commit_line(c: &CommitInfo, indent: &str, width: usize) -> Line<'static> {
    let age = c.time.map_or(String::new(), |t| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        age_label((now - t).max(0))
    });
    let prefix = format!("{indent}{age:>4} {} ", c.short_id);
    let author = format!("  {}", c.author);
    let avail = width.saturating_sub(prefix.width());
    let author_w = author.width();
    let (summary_w, author) = if avail >= author_w + MIN_SUMMARY {
        (avail - author_w, author)
    } else {
        (avail, String::new())
    };
    let summary = clip(&c.summary, summary_w);
    let pad = if author.is_empty() {
        0
    } else {
        summary_w.saturating_sub(summary.width())
    };
    Line::from(vec![
        Span::styled(prefix, dim()),
        Span::raw(summary),
        Span::raw(" ".repeat(pad)),
        Span::styled(author, dim()),
    ])
}

/// Fewest summary columns worth keeping next to an author.
const MIN_SUMMARY: usize = 20;

/// Cut `s` to `width` terminal columns, keeping the head and marking the cut.
/// Columns, not chars: an emoji takes two and would otherwise wrap the row.
fn clip(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn draw_detail(frame: &mut Frame, app: &App, detail: Option<&RepoDetail>, area: Rect) -> u16 {
    let block = || {
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_style())
            .padding(Padding::horizontal(1))
            .title(" Detail ")
            .title_style(theme::title_style())
    };
    let Some(sel) = app.selected() else {
        frame.render_widget(Paragraph::new("").block(block()), area);
        return 0;
    };

    let mut lines: Vec<Line> = vec![
        Line::from(name_spans(sel)),
        Line::from(Span::styled(sel.path.display().to_string(), dim())),
    ];
    if let Some(age) = sel.fetch_age {
        let ago = match age_label(age.as_secs() as i64).as_str() {
            "now" => "just now".to_string(),
            label => format!("{label} ago"),
        };
        lines.push(Line::from(Span::styled(
            format!("last fetch: {ago}"),
            dim(),
        )));
    }
    lines.push(Line::raw(""));

    if let Some(e) = &sel.error {
        lines.push(Line::from(Span::styled(
            e.clone(),
            facet_style(Facet::Error),
        )));
    }

    let Some(d) = detail else {
        lines.push(Line::from(Span::styled("loading…", dim())));
        frame.render_widget(
            Paragraph::new(lines)
                .block(block())
                .wrap(Wrap { trim: false }),
            area,
        );
        return 0;
    };

    // The pane's text width: its borders and horizontal padding.
    let inner_w = usize::from(area.width).saturating_sub(4);

    lines.push(section("Branches"));
    for b in &d.branches {
        let mut spans = vec![
            Span::raw(if b.is_head { "* " } else { "  " }),
            Span::raw(b.name.clone()),
        ];
        if let Some(up) = &b.upstream {
            spans.push(Span::styled(format!(" → {up}"), dim()));
        }
        if b.ahead > 0 {
            spans.push(Span::styled(
                format!(" ↑{}", b.ahead),
                facet_style(Facet::Ahead),
            ));
        }
        if b.behind > 0 {
            spans.push(Span::styled(
                format!(" ↓{}", b.behind),
                facet_style(Facet::Behind),
            ));
        }
        if b.upstream.is_none() {
            spans.push(Span::styled(" ⊘", facet_style(Facet::NoUpstream)));
        }
        lines.push(Line::from(spans));
    }

    if !d.remotes.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section("Remotes"));
        for r in &d.remotes {
            lines.push(Line::from(Span::styled(
                format!("  {} {}", r.name, r.url),
                dim(),
            )));
        }
    }

    if !d.changed_files.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section("Changes"));
        for (facet, name) in &d.changed_files {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", facet.glyph()), facet_style(*facet)),
                Span::raw(name.clone()),
            ]));
        }
    }

    if !d.stashes.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section("Stashes"));
        for s in &d.stashes {
            lines.push(Line::from(Span::styled(
                format!("  ⚑{} {}", s.index, s.message),
                facet_style(Facet::Stashed),
            )));
        }
    }

    if let Some(inc) = &d.incoming {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            section_span("Incoming"),
            Span::raw("  "),
            Span::styled(inc.range.clone(), accent()),
        ]));
        lines.push(shortcuts(&[("Shift+d", "diff"), ("Shift+l", "log")]));

        lines.push(subsection("Diff summary"));
        let max = inc
            .files
            .iter()
            .map(|f| f.added + f.removed)
            .max()
            .unwrap_or(0);
        let count_w = max.to_string().len();
        // Paths get whatever the row leaves after the indent, count and bar.
        let bar_w = BAR_MAX.min(inner_w.saturating_sub(28));
        let name_w = inc
            .files
            .iter()
            .map(|f| f.path.chars().count())
            .max()
            .unwrap_or(0)
            .min(inner_w.saturating_sub(count_w + bar_w + 6));
        for f in &inc.files {
            let mut spans = vec![
                Span::raw("    "),
                Span::raw(format!("{:name_w$} ", shorten_path(&f.path, name_w))),
                Span::styled(format!("{:>count_w$} ", f.added + f.removed), dim()),
            ];
            spans.extend(stat_bar(f, max, bar_w));
            lines.push(Line::from(spans));
        }
        if inc.truncated {
            lines.push(Line::from(Span::styled("    …", dim())));
        }
        let totals = format!(
            "    {} file{}, {} insertions(+), {} deletions(-)",
            inc.files.len(),
            if inc.files.len() == 1 { "" } else { "s" },
            inc.total_added,
            inc.total_removed
        );
        lines.push(Line::from(Span::styled(clip(&totals, inner_w), dim())));

        lines.push(Line::raw(""));
        lines.push(subsection("Commits"));
        if inc.total_commits > inc.commits.len() {
            let note = format!(
                "    ⚠ newest {} of {} shown — raise incoming_commits in the config to see more",
                inc.commits.len(),
                inc.total_commits
            );
            lines.push(Line::from(Span::styled(clip(&note, inner_w), warn())));
        }
        for c in &inc.commits {
            lines.push(commit_line(c, "    ", inner_w));
        }
    }

    if !d.recent.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section("Recent"));
        for c in &d.recent {
            lines.push(commit_line(c, "  ", inner_w));
        }
    }

    scrollable(frame, area, "Detail", lines, app.pane_scroll())
}

fn draw_problems(frame: &mut Frame, app: &App, area: Rect) -> u16 {
    let lines: Vec<Line> = if app.problems().is_empty() {
        vec![Line::from(Span::styled("No problems.", dim()))]
    } else {
        app.problems()
            .iter()
            .map(|p| {
                Line::from(vec![
                    Span::styled(
                        format!("{} ", Facet::Error.glyph()),
                        facet_style(Facet::Error),
                    ),
                    Span::raw(format!("{}: {}", p.path.display(), p.reason)),
                ])
            })
            .collect()
    };
    scrollable(
        frame,
        area,
        "Problems (! or Esc to close)",
        lines,
        app.pane_scroll(),
    )
}

fn draw_confirm(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .padding(Padding::horizontal(1))
        .title(" Confirm ")
        .title_style(theme::title_style());
    let Some(pending) = app.pending() else {
        frame.render_widget(Paragraph::new("").block(block), area);
        return;
    };
    let mut lines = vec![
        Line::raw(""),
        Line::from(Span::styled(
            format!("About to {}.", pending.summary),
            facet_style(Facet::Conflict),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "This cannot be undone.",
            facet_style(Facet::Conflict),
        )),
        Line::raw(""),
    ];
    for path in pending.targets.iter().take(10) {
        lines.push(Line::from(Span::styled(
            format!("  {}", path.display()),
            dim(),
        )));
    }
    if pending.targets.len() > 10 {
        lines.push(Line::from(Span::styled(
            format!("  … and {} more", pending.targets.len() - 10),
            dim(),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("y", accent().add_modifier(Modifier::BOLD)),
        Span::raw(" to proceed · any other key cancels"),
    ]));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The box over the dimmed screen while a modal job runs.
fn draw_progress(frame: &mut Frame, job: &Job, area: Rect) {
    let popup = centered(area, 52, 7);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .padding(Padding::horizontal(1))
        .title(format!(" {} ", job.label))
        .title_style(theme::title_style());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let spinner = spinner_frame(since_start());
    let secs = job.started.elapsed().as_secs();
    let mut detail = format!("{secs}s");
    if job.failed > 0 {
        detail = format!("{} failed  ·  {detail}", job.failed);
    }
    let mut lines = vec![Line::raw(""), Line::raw("")];
    let done = job.done.min(job.total.max(job.done));
    if job.total == 0 {
        lines[0] = Line::from(Span::styled(format!("{spinner} {done}"), accent()));
    } else {
        let count = format!("{done}/{}  {}%", job.total, done * 100 / job.total);
        let bar_w = (inner.width as usize)
            .saturating_sub(count.width() + 3)
            .max(1);
        let filled = (done * bar_w / job.total).min(bar_w);
        lines[0] = Line::from(vec![
            Span::styled(format!("{spinner} "), accent()),
            Span::styled("█".repeat(filled), accent()),
            Span::styled("░".repeat(bar_w - filled), dim()),
            Span::raw(format!(" {count}")),
        ]);
    }
    lines[1] = Line::from(Span::styled(detail, dim()));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A box of `width` x `height`, centred in `area` and never larger than it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Repositories sitting directly in a root have no namespace to name.
fn namespace_label(name: &str) -> &str {
    if name.is_empty() {
        "(root)"
    } else {
        name
    }
}

/// The one popover renderer. Callers differ only in title, row text and footer.
fn draw_picker(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: Vec<Line<'static>>,
    index: usize,
    footer: &str,
) {
    let rows: Vec<Line<'static>> = rows
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let mut spans = vec![cursor(i == index), Span::raw(" ")];
            spans.extend(l.spans);
            Line::from(spans)
        })
        .collect();
    let body_w = rows
        .iter()
        .map(|l| l.width())
        .chain([title.chars().count(), footer.chars().count()])
        .max()
        .unwrap_or(10);
    let rows: Vec<Line<'static>> = rows
        .into_iter()
        .enumerate()
        .map(|(i, l)| if i == index { banded(l, body_w) } else { l })
        .collect();
    let width = (body_w + 4) as u16;
    let height = (rows.len() + 3) as u16;
    let popup = centered(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .padding(Padding::horizontal(1))
        .title(format!(" {title} "))
        .title_style(theme::title_style());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let list = Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let mut state = ListState::default();
    state.select(Some(index));
    frame.render_stateful_widget(
        List::new(rows.into_iter().map(ListItem::new).collect::<Vec<_>>()),
        list,
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer.to_string(), dim()))),
        Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: 1,
            ..inner
        },
    );
}

fn draw_namespaces(frame: &mut Frame, app: &App, area: Rect) {
    let choices = app.namespace_choices();
    let name_w = choices
        .iter()
        .map(|(n, _)| namespace_label(n).chars().count())
        .max()
        .unwrap_or(3);
    let count_w = choices
        .iter()
        .map(|(_, c)| c.to_string().len())
        .max()
        .unwrap_or(1);

    let rows: Vec<Line<'static>> = choices
        .iter()
        .map(|(name, count)| {
            let active = app.group().unwrap_or("all") == name;
            let label = namespace_label(name);
            let mut style = Style::default().add_modifier(Modifier::BOLD);
            if active {
                style = style.fg(theme::Color::Accent.to_ratatui());
            }
            Line::from(vec![
                Span::styled(if active { "● " } else { "  " }, accent()),
                Span::styled(format!("{label:name_w$}"), style),
                Span::styled(format!("  {count:>count_w$}"), dim()),
            ])
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Namespace",
        rows,
        app.picker_index(),
        "↑↓ move   Enter choose   Esc close",
    );
}

fn draw_sort(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Line<'static>> = drift::Sort::ALL
        .iter()
        .map(|s| {
            let active = *s == app.sort_mode();
            let mut style = Style::default();
            if active {
                style = style
                    .fg(theme::Color::Accent.to_ratatui())
                    .add_modifier(Modifier::BOLD);
            }
            Line::from(vec![
                Span::styled(if active { "● " } else { "  " }, accent()),
                Span::styled(format!("{:<12}", s.label()), style),
                Span::styled(s.order().to_string(), dim()),
            ])
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Sort by",
        rows,
        app.picker_index(),
        "↑↓ move   Enter choose   Esc close",
    );
}

fn draw_branches(frame: &mut Frame, app: &App, area: Rect) {
    let name_w = app
        .branch_choices()
        .iter()
        .map(|b| b.name.chars().count())
        .max()
        .unwrap_or(6);

    let rows: Vec<Line<'static>> = app
        .branch_choices()
        .iter()
        .map(|b| {
            let mut spans = vec![
                Span::styled(if b.is_head { "● " } else { "  " }, accent()),
                Span::raw(format!("{:name_w$}", b.name)),
            ];
            if b.ahead > 0 {
                spans.push(Span::styled(
                    format!(" ↑{}", b.ahead),
                    facet_style(Facet::Ahead),
                ));
            }
            if b.behind > 0 {
                spans.push(Span::styled(
                    format!(" ↓{}", b.behind),
                    facet_style(Facet::Behind),
                ));
            }
            if !b.has_upstream {
                spans.push(Span::styled(" ⊘", facet_style(Facet::NoUpstream)));
            }
            Line::from(spans)
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Switch branch",
        rows,
        app.picker_index(),
        "↑↓ move   Enter switch   d diff   x delete   Esc close",
    );
}

fn draw_stashes(frame: &mut Frame, app: &App, area: Rect) {
    // The popover is centred, so it can use everything but its own frame.
    let msg_w = usize::from(area.width).saturating_sub(14);
    let rows: Vec<Line<'static>> = app
        .stash_choices()
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled(format!("  ⚑{:<3}", s.index), facet_style(Facet::Stashed)),
                Span::raw(elide(&s.message, msg_w)),
            ])
        })
        .collect();

    draw_picker(
        frame,
        area,
        "Stashes",
        rows,
        app.picker_index(),
        "↑↓ move   Enter view   x drop   Esc close",
    );
}

/// Grouped by which view each binding applies to; General last so it stays
/// reachable at the bottom of a scrolled pane. Also read by the cross-check test.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "List view — navigate",
        &[
            ("↑↓", "Move"),
            ("g / Shift+g", "First / last"),
            ("n", "Filter by namespace"),
            (
                "Shift+s",
                "Open the sort picker: Drift, Name, Last commit, Last fetch, # Branches",
            ),
            (
                "o",
                "Re-sort now (the list otherwise keeps its order after a scan)",
            ),
            ("d", "Drifted only"),
            ("/", "Filter"),
        ],
    ),
    (
        "List view — mark",
        &[
            ("Space", "Toggle the mark and move down"),
            ("Ctrl+Space", "Mark from the last toggled row to the cursor"),
            ("a", "Mark all visible, or clear the marks"),
        ],
    ),
    (
        "List view — act (on the marks, else the current repo)",
        &[
            ("f / Shift+f", "Fetch marked-or-current / fetch all"),
            (
                "p / Shift+p",
                "Pull (honours your pull.rebase config), marked-or-current / all drifted",
            ),
            ("Shift+x", "Prune gone branches (confirmed)"),
            ("Enter", "Shell in repo"),
            ("l", "Open lazygit in repo"),
            ("w", "Open the repository on the web"),
            ("Shift+d", "Diff HEAD...@{u} in your pager"),
            ("Shift+l", "Log of the incoming commits, in your pager"),
            ("Shift+m", "Diff against the default branch, in your pager"),
            ("b", "Switch branch on the current repo"),
            ("s", "Browse stashes: view, drop"),
            ("r", "Rescan"),
        ],
    ),
    (
        "Branch popover",
        &[
            (
                "d",
                "Diff the highlighted branch against the default branch",
            ),
            ("x", "Delete the highlighted branch (confirmed)"),
        ],
    ),
    (
        "Stash popover",
        &[("x", "Drop the selected stash (confirmed)")],
    ),
    (
        "Detail pane",
        &[
            ("z", "Zoom the detail pane to full screen (Esc / z back)"),
            ("Shift+j / Shift+k", "Scroll the detail pane"),
            ("PgUp/PgDn", "Scroll the detail pane a page"),
            (
                "wheel",
                "Scroll the detail under the pointer, else the list",
            ),
            (
                "Tab",
                "Show/hide it: beside the list when every name fits there, full-screen otherwise",
            ),
        ],
    ),
    (
        "General",
        &[
            ("!", "Problems"),
            ("?", "This help"),
            ("Esc", "Close the zoom, filter or pane first, then quit"),
            ("q / Ctrl-C", "Quit"),
        ],
    ),
];

/// One colour per `HELP` section, cycled the way terragrunt tints each
/// unit's log output so sections stay visually separable while scrolling.
const SECTION_COLORS: &[theme::Color] = &[
    theme::Color::Teal,
    theme::Color::Mauve,
    theme::Color::Pink,
    theme::Color::Blue,
    theme::Color::Yellow,
    theme::Color::DimGreen,
];

fn draw_help(frame: &mut Frame, app: &App, area: Rect) -> u16 {
    let key_w = HELP
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let mut lines: Vec<Line> = Vec::new();
    for (i, (title, rows)) in HELP.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        let color = SECTION_COLORS[i % SECTION_COLORS.len()];
        let title_style = Style::default()
            .fg(color.to_ratatui())
            .add_modifier(Modifier::BOLD);
        let key_style = Style::default().fg(color.to_ratatui());
        lines.push(Line::from(Span::styled(title.to_string(), title_style)));
        for (k, v) in *rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:key_w$}"), key_style),
                Span::raw(*v),
            ]));
        }
    }
    scrollable(
        frame,
        area,
        "Help (? or Esc to close)",
        lines,
        app.pane_scroll(),
    )
}

/// The one hints row, most important first; dropped from the right when the
/// terminal is too narrow to hold them all.
const HINTS: &[(&str, &str)] = &[
    ("↑↓", "move"),
    ("/", "filter"),
    ("Space", "mark"),
    ("f", "fetch"),
    ("p", "pull"),
    ("Tab", "detail"),
    ("n", "namespace"),
    ("Shift+s", "sort"),
];

/// Pinned at the end of the hints row; never dropped for width.
const TAIL: &str = "? help | Esc quit";

/// As many labelled hints as fit in `width`, leaving room for `tail`.
fn fit_hints(hints: &[(&str, &str)], tail: &str, width: usize) -> String {
    const DELIM: &str = " | ";
    let reserved = if tail.is_empty() {
        0
    } else {
        DELIM.len() + tail.len()
    };
    let mut out = String::new();
    let mut used = 0usize;
    for (key, label) in hints {
        let piece = format!("{key} {label}");
        let extra = piece.chars().count() + if used == 0 { 0 } else { DELIM.len() };
        if used + extra + reserved > width {
            break;
        }
        if used > 0 {
            out.push_str(DELIM);
        }
        out.push_str(&piece);
        used += extra;
    }
    if !tail.is_empty() {
        if used > 0 {
            out.push_str(DELIM);
        }
        out.push_str(tail);
    }
    out
}

/// One glyph and count per non-zero facet, by the same `theme::worst` rule
/// that colours a repo's name in the list.
fn drift_tally(rows: &[&RepoStatus]) -> Vec<Span<'static>> {
    let mut counts = [0usize; Facet::ALL.len()];
    for r in rows {
        if let Some(i) = Facet::ALL.iter().position(|f| *f == theme::worst(r)) {
            counts[i] += 1;
        }
    }
    let mut spans = Vec::new();
    for (facet, count) in Facet::ALL.iter().zip(counts) {
        if *facet == Facet::Clean || count == 0 {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("{}{count}", facet.glyph()),
            facet_style(*facet),
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            format!("{} all clean", Facet::Clean.glyph()),
            facet_style(Facet::Clean),
        ));
    }
    spans
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn spinner_frame(elapsed: Duration) -> char {
    SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()]
}

/// Time since the first frame. The loop already redraws every 80 ms, so the
/// animation needs no counter threaded through the state.
fn since_start() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// One line naming the work in flight, or `None` when the screen is current.
fn activity(jobs: &[Job], frame: char) -> Option<String> {
    let first = jobs.first()?;
    let count = if first.total == 0 {
        first.done.to_string()
    } else {
        format!("{}/{}", first.done, first.total)
    };
    let extra = match jobs.len() {
        1 => String::new(),
        n => format!(" +{}", n - 1),
    };
    Some(format!("{frame} {} {count}{extra}", first.label))
}

/// Drift tally with the marks and activity (job/toast/filter) beside it, then
/// the keybinding hints.
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .padding(Padding::horizontal(1))
        .title(" gitdrift ")
        .title_style(theme::title_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Several toasts land while a job is still running, the stash-drop
    // recovery id among them; the spinner must not swallow them.
    let activity_spans = if let Some(filter) = app.filter_text() {
        vec![
            Span::styled("/", accent()),
            Span::raw(filter.to_string()),
            Span::styled("▏", accent()),
        ]
    } else if let Some(text) = activity(app.jobs(), spinner_frame(since_start())) {
        match app.toast() {
            Some(toast) => vec![Span::styled(format!("{text}  {toast}"), accent())],
            None => vec![Span::styled(text, accent())],
        }
    } else if let Some(toast) = app.toast() {
        vec![Span::styled(toast.to_string(), accent())]
    } else {
        Vec::new()
    };

    let w = inner.width as usize;
    let mut tally = drift_tally(&app.visible());
    if app.marked_count() > 0 {
        tally.push(Span::styled(
            format!(" · {} marked", app.marked_count()),
            accent(),
        ));
    }
    if !activity_spans.is_empty() {
        tally.push(Span::raw("  "));
        tally.extend(activity_spans);
    }
    let lines = vec![
        Line::from(tally),
        Line::from(Span::styled(fit_hints(HINTS, TAIL, w), dim())),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::Incoming;
    use crate::status::Head;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn row(name: &str, ahead: u32) -> RepoStatus {
        RepoStatus {
            path: PathBuf::from("/tmp").join(name),
            display_name: name.to_string(),
            head: Head::Branch("main".into()),
            upstream: Some("origin/main".into()),
            ahead,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            stash_count: 0,
            local_branches: 0,
            last_commit_time: Some(1),
            fetch_age: None,
            error: None,
        }
    }

    fn render(width: u16, height: u16, app: &App) -> String {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut state = ListState::default();
        term.draw(|f| {
            draw(f, app, None, &mut state);
        })
        .unwrap();
        buffer_string(term.backend().buffer())
    }

    fn buffer_string(buf: &ratatui::buffer::Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(c: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c))
    }

    fn app_with(rows: Vec<RepoStatus>) -> App {
        let mut a = App::new();
        for r in rows {
            a.push_status(r);
        }
        a.finish_scan();
        a
    }

    #[test]
    fn the_list_shows_repository_names_and_their_glyphs() {
        let app = app_with(vec![row("sre/alpha", 2)]);
        let out = render(120, 20, &app);
        assert!(out.contains("sre/alpha"), "{out}");
        assert!(out.contains('↑'), "{out}");
    }

    #[test]
    fn the_list_pane_has_a_column_header_above_the_rows() {
        let app = app_with(vec![row("sre/alpha", 1)]);
        let out = render(120, 20, &app);
        assert!(out.contains("NAME"), "{out}");
        assert!(out.contains("BRANCH"), "{out}");
        assert!(out.contains("#BR"), "{out}");
        assert!(out.contains("DRIFT"), "{out}");
        let header_row = out.lines().position(|l| l.contains("NAME")).unwrap();
        // "sre/alpha" alone also matches the detail pane's title; "main" (its
        // branch) only appears on the list row itself.
        let repo_row = out
            .lines()
            .position(|l| l.contains("sre/alpha") && l.contains("main"))
            .unwrap();
        assert!(
            header_row < repo_row,
            "the header sits above the rows\n{out}"
        );
    }

    #[test]
    fn a_repository_name_too_long_for_the_side_pane_drops_it_rather_than_squashing_it() {
        let fits = app_with(vec![row("sre/alpha", 1)]);
        let truncates = app_with(vec![row(&"x".repeat(200), 1)]);
        let short = render(260, 20, &fits);
        let long = render(260, 20, &truncates);
        assert!(
            short.contains("Detail"),
            "a name that fits gets the side pane:\n{short}"
        );
        assert!(
            !long.contains("Detail"),
            "a name too long for it drops to standard instead:\n{long}"
        );
    }

    #[test]
    fn a_long_namespace_does_not_force_standard_when_the_repository_name_itself_is_short() {
        // shorten_path collapses the namespace before ever touching the name;
        // only the name's own length should decide the mode.
        let deep = "idp/developer-control-plane/workflow-automation-n8n/unified-content-review-training-dataset/short-name";
        let app = app_with(vec![row(deep, 1)]);
        let out = render(260, 20, &app);
        assert!(
            out.contains("Detail"),
            "the path is long, the name is not:\n{out}"
        );
    }

    #[test]
    fn tab_hides_the_side_pane_when_a_name_fits() {
        let mut app = app_with(vec![row("sre/alpha", 1)]);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(260, 20, &app);
        assert!(!out.contains("Detail"), "{out}");
        assert!(
            out.contains("repositories"),
            "the list still fills the screen:\n{out}"
        );
    }

    #[test]
    fn tab_opens_the_detail_full_screen_when_a_name_does_not_fit() {
        let mut app = app_with(vec![row(&"x".repeat(200), 1)]);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(260, 20, &app);
        assert!(out.contains("Detail"), "{out}");
        assert!(
            !out.contains("repositories"),
            "it replaces the list, it does not float over it:\n{out}"
        );
    }

    #[test]
    fn the_footer_shows_keybindings() {
        let app = app_with(vec![row("a", 1)]);
        let out = render(120, 20, &app);
        assert!(out.contains("move"), "{out}");
        assert!(out.contains("quit"), "{out}");
        assert!(out.contains("Tab detail"), "{out}");
    }

    #[test]
    fn scanning_is_announced_while_the_scan_runs() {
        let mut app = App::new();
        app.push_status(row("a", 1));
        let out = render(120, 20, &app);
        assert!(out.to_lowercase().contains("scanning"), "{out}");
    }

    #[test]
    fn a_toast_replaces_the_keybinding_hints_in_the_footer() {
        let mut app = app_with(vec![row("a", 1)]);
        app.set_toast("fetched 3 repos", std::time::Duration::from_secs(10));
        let out = render(120, 20, &app);
        assert!(out.contains("fetched 3 repos"), "{out}");
    }

    #[test]
    fn a_toast_is_still_rendered_while_a_job_is_in_flight() {
        let mut app = app_with(vec![row("a", 1)]);
        app.begin_job("fetching", 3);
        app.set_toast(
            "Dropped refs/stash@{0} (a1b2c3d)",
            std::time::Duration::from_secs(10),
        );
        let out = render(120, 20, &app);
        assert!(out.contains("a1b2c3d"), "{out}");
        assert!(out.contains("fetching"), "the spinner stays too:\n{out}");
    }

    fn job(label: &str, done: usize, total: usize) -> Job {
        Job {
            done,
            ..Job::new(0, label, total)
        }
    }

    #[test]
    fn a_modal_job_dims_the_screen_and_shows_its_progress() {
        let mut app = app_with(vec![row("alpha", 1)]);
        app.finish_scan();
        let id = app.begin_modal_job("fetching", 4);
        app.advance_job(id);
        app.fail_job(id);
        let out = render(100, 20, &app);
        assert!(out.contains("2/4  50%"), "{out}");
        assert!(out.contains("1 failed"), "{out}");
        assert!(
            out.contains("alpha"),
            "the list stays visible behind it:\n{out}"
        );
    }

    #[test]
    fn the_activity_line_shows_the_label_and_its_progress() {
        let jobs = [job("fetching", 37, 511)];
        assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ fetching 37/511");
    }

    #[test]
    fn an_indeterminate_job_shows_only_what_it_has_done() {
        let jobs = [job("scanning", 214, 0)];
        assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ scanning 214");
    }

    #[test]
    fn extra_jobs_are_counted_not_listed() {
        let jobs = [job("fetching", 1, 2), job("pruning", 0, 4)];
        assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ fetching 1/2 +1");
    }

    #[test]
    fn there_is_no_activity_line_when_nothing_is_running() {
        assert!(activity(&[], '⠹').is_none());
    }

    #[test]
    fn the_spinner_advances_over_time() {
        assert_ne!(
            spinner_frame(Duration::from_millis(0)),
            spinner_frame(Duration::from_millis(100))
        );
    }

    #[test]
    fn the_filter_is_echoed_in_the_footer_while_typing() {
        let mut app = app_with(vec![row("alpha", 1)]);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(120, 20, &app);
        assert!(out.contains("/a"), "{out}");
    }

    #[test]
    fn the_help_pane_is_rendered() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('?'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.pane(), Pane::Help);
        let out = render(120, 30, &app);
        assert!(out.contains("pull.rebase"), "{out}");
    }

    #[test]
    fn the_list_scroll_offset_persists_across_frames() {
        let rows: Vec<RepoStatus> = (0..60).map(|i| row(&format!("repo{i:02}"), 1)).collect();
        let mut app = app_with(rows);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('G'),
            crossterm::event::KeyModifiers::SHIFT,
        ));

        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut state = ListState::default();

        term.draw(|f| {
            draw(f, &app, None, &mut state);
        })
        .unwrap();
        let first = buffer_string(term.backend().buffer());

        term.draw(|f| {
            draw(f, &app, None, &mut state);
        })
        .unwrap();
        let second = buffer_string(term.backend().buffer());

        assert_eq!(
            first, second,
            "the viewport should not shift between identical frames"
        );
    }

    #[test]
    fn an_empty_result_set_says_so_instead_of_rendering_nothing() {
        let mut app = App::new();
        app.finish_scan();
        let out = render(120, 20, &app);
        assert!(out.to_lowercase().contains("no repositories"), "{out}");
    }

    #[test]
    fn the_confirm_pane_names_the_action_and_warns_it_is_final() {
        let mut app = app_with(vec![row("sre/alpha", 1)]);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('X'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(120, 24, &app);
        assert!(out.contains("prune remotes"), "{out}");
        assert!(out.contains("cannot be undone"), "{out}");
        assert!(out.contains("y"), "{out}");
    }

    #[test]
    fn an_applied_filter_and_drifted_only_are_named_in_the_list_title() {
        let mut app = app_with(vec![row("alpha", 1), row("beta", 1)]);
        for c in ['/', 'a', 'l'] {
            app.on_key(key(c));
        }
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        app.on_key(key('d'));
        let out = render(120, 20, &app);
        assert!(out.contains("· /al"), "{out}");
        assert!(out.contains("drifted only"), "{out}");
    }

    #[test]
    fn the_header_counts_marked_repositories() {
        let mut app = app_with(vec![row("a", 1), row("b", 1)]);
        app.on_key(key(' '));
        let out = render(120, 20, &app);
        assert!(out.contains("1 marked"), "{out}");
    }

    #[test]
    fn marked_rows_are_flagged_in_the_list() {
        let mut app = app_with(vec![row("sre/alpha", 1)]);
        let before = render(120, 20, &app);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ));
        let after = render(120, 20, &app);
        assert_ne!(before, after, "a marked row must look different");
        assert!(after.contains('◆'), "{after}");
    }

    fn detail_with_commits(n: usize) -> RepoDetail {
        RepoDetail {
            recent: (0..n)
                .map(|i| crate::detail::CommitInfo {
                    short_id: format!("abc{i:04}"),
                    summary: format!("commit {i}"),
                    author: "me".into(),
                    time: None,
                })
                .collect(),
            ..RepoDetail::default()
        }
    }

    fn render_detail(height: u16, app: &App, d: &RepoDetail) -> (Rendered, String) {
        let mut term = Terminal::new(TestBackend::new(260, height)).unwrap();
        let mut state = ListState::default();
        let mut rendered = Rendered::default();
        term.draw(|f| rendered = draw(f, app, Some(d), &mut state))
            .unwrap();
        (rendered, buffer_string(term.backend().buffer()))
    }

    #[test]
    fn the_help_pane_scrolls_when_the_terminal_is_too_short_to_hold_it() {
        let mut app = app_with(vec![row("a", 1)]);
        app.on_key(key('?'));

        let mut term = Terminal::new(TestBackend::new(120, 12)).unwrap();
        let mut state = ListState::default();
        let mut rendered = Rendered::default();
        term.draw(|f| rendered = draw(f, &app, None, &mut state))
            .unwrap();
        assert!(
            rendered.pane_max_scroll > 0,
            "the help does not fit in 12 rows"
        );
        let out = buffer_string(term.backend().buffer());
        assert!(out.contains('▾'), "{out}");

        for _ in 0..60 {
            app.on_key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Down,
            ));
        }
        term.draw(|f| rendered = draw(f, &app, None, &mut state))
            .unwrap();
        let out = buffer_string(term.backend().buffer());
        assert!(out.contains("Ctrl-C"), "the last row is reachable:\n{out}");
        assert!(out.contains('▴') && !out.contains('▾'), "{out}");
    }

    #[test]
    fn detail_branches_show_their_upstream() {
        let app = app_with(vec![row("a", 1)]);
        let d = RepoDetail {
            branches: vec![
                crate::detail::BranchInfo {
                    name: "main".into(),
                    upstream: Some("origin/main".into()),
                    ahead: 2,
                    behind: 0,
                    is_head: true,
                },
                crate::detail::BranchInfo {
                    name: "wip".into(),
                    upstream: None,
                    ahead: 0,
                    behind: 0,
                    is_head: false,
                },
            ],
            ..RepoDetail::default()
        };
        let (_, out) = render_detail(30, &app, &d);
        assert!(out.contains("* main → origin/main ↑2"), "{out}");
        assert!(out.contains("  wip ⊘"), "{out}");
    }

    #[test]
    fn a_detail_pane_that_fits_reports_no_scroll_and_shows_no_arrow() {
        let app = app_with(vec![row("a", 1)]);
        let (rendered, out) = render_detail(30, &app, &detail_with_commits(1));
        assert_eq!(rendered.pane_max_scroll, 0);
        assert!(!out.contains('▾'), "nothing below:\n{out}");
    }

    #[test]
    fn an_overflowing_detail_pane_bounds_the_scroll_and_points_the_way() {
        let app = app_with(vec![row("a", 1)]);
        let d = detail_with_commits(40);
        let (rendered, out) = render_detail(20, &app, &d);
        assert!(rendered.pane_max_scroll > 0, "there is more below");
        assert!(out.contains('▾'), "and the title says so:\n{out}");

        // Scrolling past the end still lands on the last screenful.
        let mut app = app;
        app.scroll_pane_by(500);
        let (_, out) = render_detail(20, &app, &d);
        assert!(
            out.contains("commit 39"),
            "the last commit is on screen:\n{out}"
        );
        assert!(out.contains('▴') && !out.contains('▾'), "{out}");
    }

    #[test]
    fn the_selected_row_is_marked_by_a_gutter_bar_and_keeps_its_own_colours() {
        let app = app_with(vec![row("a", 1), row("b", 1)]);
        let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
        let mut state = ListState::default();
        let mut rendered = Rendered::default();
        term.draw(|f| {
            rendered = draw(f, &app, None, &mut state);
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Bound checks to the list pane's own columns: the detail pane next
        // to it renders independently and may carry dim text of its own.
        let list_width = rendered.detail_area.map_or(buf.area.width, |r| r.x);

        let bars: Vec<u16> = (0..buf.area.height)
            .filter(|y| (0..list_width).any(|x| buf[(x, *y)].symbol() == "▌"))
            .collect();
        assert_eq!(bars.len(), 1, "exactly one row carries the cursor");

        let y = bars[0];
        for x in 0..list_width {
            let cell = &buf[(x, y)];
            assert!(
                !cell.modifier.contains(Modifier::REVERSED),
                "the selected row must not be inverted at column {x}"
            );
        }
        // The band runs the width of the pane, inside its border and padding.
        for x in 2..buf.area.width / 2 {
            assert_eq!(
                buf[(x, y)].bg,
                selection_bg(),
                "the band has a hole at column {x}"
            );
        }
        assert!(
            (0..list_width).all(|x| buf[(x, y)].fg != theme::Color::Dim.to_ratatui()),
            "dim text would vanish into the band"
        );
        let ahead = (0..list_width)
            .map(|x| &buf[(x, y)])
            .find(|c| c.symbol() == "↑")
            .expect("the ahead glyph is on the selected row");
        assert_eq!(
            ahead.fg,
            Facet::Ahead.color().to_ratatui(),
            "status colours survive selection"
        );
    }

    #[test]
    fn the_branch_column_is_aligned_across_rows() {
        let app = app_with(vec![row("a", 1), row("a-much-longer-repo-name", 1)]);
        let out = render(120, 20, &app);
        let cols: Vec<usize> = out
            .lines()
            .filter(|l| l.contains("main"))
            .map(|l| l[..l.find("main").unwrap()].chars().count())
            .collect();
        assert_eq!(cols.len(), 2, "both rows rendered");
        assert_eq!(cols[0], cols[1], "branch starts in the same column:\n{out}");
    }

    #[test]
    fn a_long_name_is_shortened_rather_than_pushing_the_status_column_off_screen() {
        let long = "idp/developer-control-plane/workflow-automation-n8n/unified-content-review-training-dataset/datatables";
        let app = app_with(vec![row("a", 1), row(long, 7)]);
        let out = render(80, 20, &app);
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with('│') && l.contains('↑') && l.contains("main"))
            .collect();
        assert_eq!(rows.len(), 2, "both rows keep their status cell:\n{out}");
        // Columns, not byte offsets: the elision marker is multi-byte.
        let cols: Vec<usize> = rows
            .iter()
            .map(|l| l.chars().position(|c| c == '↑').unwrap())
            .collect();
        assert_eq!(cols[0], cols[1], "status column aligned:\n{out}");
        assert!(
            out.contains("idp/"),
            "the head of the path survives:\n{out}"
        );
        assert!(
            out.contains("…/datatables"),
            "the repository name is never truncated:\n{out}"
        );
    }

    #[test]
    fn branch_names_are_never_truncated() {
        let mut long_branch = row("a", 1);
        long_branch.head = Head::Branch("test/precommit-validate-commit-messages".into());
        let app = app_with(vec![
            long_branch,
            row("some/deeply/nested/namespace/path/repo", 1),
        ]);
        let out = render(140, 20, &app);
        assert!(
            out.contains("test/precommit-validate-commit-messages"),
            "the branch column fits the longest branch whole:\n{out}"
        );
    }

    #[test]
    fn the_namespace_picker_floats_over_the_list_instead_of_replacing_it() {
        let mut app = app_with(vec![row("idp/one", 1), row("mkp/two", 1)]);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('n'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(120, 24, &app);
        assert!(out.contains("Namespace"), "{out}");
        assert!(out.contains("all"), "{out}");
        assert!(out.contains("idp"), "{out}");
        assert!(
            out.contains("repositories"),
            "the list is still visible behind it:\n{out}"
        );
    }

    #[test]
    fn rendering_a_tiny_terminal_does_not_panic() {
        let app = app_with(vec![row("a", 1)]);
        let _ = render(20, 3, &app);
    }

    #[test]
    fn the_widest_file_fills_the_bar_and_the_rest_scale_to_it() {
        let big = FileDelta {
            added: 40,
            removed: 0,
            path: "big".into(),
        };
        let small = FileDelta {
            added: 10,
            removed: 0,
            path: "small".into(),
        };
        let cells = |d| {
            stat_bar(d, 40, 20)
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        };
        assert_eq!(cells(&big), 20);
        assert_eq!(cells(&small), 5);
    }

    #[test]
    fn a_file_with_no_changed_lines_draws_no_bar() {
        let binary = FileDelta {
            added: 0,
            removed: 0,
            path: "logo.png".into(),
        };
        assert!(stat_bar(&binary, 40, 20).is_empty());
    }

    #[test]
    fn ages_are_short_and_pick_the_largest_unit() {
        let cases = [
            (0, "now"),
            (59, "now"),
            (60, "1m"),
            (3_599, "59m"),
            (3_600, "1h"),
            (86_399, "23h"),
            (86_400, "1d"),
            (364 * 86_400, "364d"),
            (365 * 86_400, "1y"),
            (545 * 86_400, "1y"),
            (4_000 * 86_400, "10y"),
        ];
        for (secs, want) in cases {
            assert_eq!(age_label(secs), want, "{secs}s");
        }
    }

    #[test]
    fn clip_keeps_the_head_and_marks_the_cut() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("abc", 0), "…");
        // A wide char never straddles the cut: 🟢 is two columns.
        assert_eq!(clip("ab🟢cd", 4), "ab…");
        assert_eq!(clip("ab🟢cd", 5), "ab🟢…");
    }

    #[test]
    fn a_long_commit_summary_is_clipped_to_one_row() {
        let app = app_with(vec![row("repo", 1)]);
        let long = "chore: update thing to version ".to_string() + &"9f".repeat(60);
        let commit = |id: &str| crate::detail::CommitInfo {
            short_id: id.into(),
            summary: long.clone(),
            author: "Ada Lovelace".into(),
            time: None,
        };
        let detail = RepoDetail {
            recent: vec![commit("aaaaaaa")],
            ..RepoDetail::default()
        };
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut state = ListState::default();
        term.draw(|f| {
            draw(f, &app, Some(&detail), &mut state);
        })
        .unwrap();
        let out = buffer_string(term.backend().buffer());
        let rows: Vec<&str> = out.lines().filter(|l| l.contains("aaaaaaa")).collect();
        assert_eq!(rows.len(), 1, "{out}");
        assert!(rows[0].contains('…'), "{out}");
        assert!(
            !out.contains("9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f"),
            "{out}"
        );
    }

    #[test]
    fn incoming_is_split_into_diff_summary_and_commits_and_warns_when_capped() {
        let app = app_with(vec![row("repo", 1)]);
        let commit = crate::detail::CommitInfo {
            short_id: "def5678".into(),
            summary: "add users endpoint".into(),
            author: "Ada".into(),
            time: None,
        };
        let detail = RepoDetail {
            incoming: Some(Incoming {
                commits: vec![commit; 3],
                total_commits: 212,
                range: "abc1234..def5678".into(),
                ..Incoming::default()
            }),
            ..RepoDetail::default()
        };
        let mut term = Terminal::new(TestBackend::new(200, 40)).unwrap();
        let mut state = ListState::default();
        term.draw(|f| {
            draw(f, &app, Some(&detail), &mut state);
        })
        .unwrap();
        let out = buffer_string(term.backend().buffer());
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{out}"))
        };
        assert!(at("Incoming") < at("Shift+d diff"));
        assert!(at("Shift+d diff") < at("Diff summary"));
        assert!(at("Diff summary") < at("Commits"));
        assert!(at("Commits") < at("newest 3 of 212 shown"));
        assert!(at("newest 3 of 212 shown") < at("def5678 add users"));
    }

    #[test]
    fn detail_sections_run_branches_remotes_changes_incoming_recent() {
        let app = app_with(vec![row("repo", 1)]);
        let commit = crate::detail::CommitInfo {
            short_id: "def5678".into(),
            summary: "add users endpoint".into(),
            author: "Ada".into(),
            time: None,
        };
        let detail = RepoDetail {
            remotes: vec![crate::detail::RemoteInfo {
                name: "origin".into(),
                url: "git@example.com:a/b.git".into(),
            }],
            changed_files: vec![(Facet::Unstaged, "src/lib.rs".into())],
            incoming: Some(Incoming {
                commits: vec![commit.clone()],
                total_commits: 1,
                ..Incoming::default()
            }),
            recent: vec![commit],
            ..RepoDetail::default()
        };
        let mut term = Terminal::new(TestBackend::new(200, 60)).unwrap();
        let mut state = ListState::default();
        term.draw(|f| {
            draw(f, &app, Some(&detail), &mut state);
        })
        .unwrap();
        let out = buffer_string(term.backend().buffer());
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{out}"))
        };
        assert!(at("Branches") < at("Remotes"));
        assert!(at("Remotes") < at("Changes"));
        assert!(at("Changes") < at("Incoming"));
        assert!(at("Incoming") < at("Recent"));
    }

    #[test]
    fn diff_summary_rows_never_wrap_at_any_pane_width() {
        let app = app_with(vec![row("repo", 1)]);
        let detail = RepoDetail {
            incoming: Some(Incoming {
                files: vec![
                    FileDelta {
                        added: 413,
                        removed: 3,
                        path:
                            "plugins/datamorph-backend/src/actions/createDatamorphActions.test.ts"
                                .into(),
                    },
                    FileDelta {
                        added: 2,
                        removed: 1,
                        path: "a.txt".into(),
                    },
                ],
                total_added: 415,
                total_removed: 4,
                ..Incoming::default()
            }),
            ..RepoDetail::default()
        };
        for width in 50..=160 {
            let mut term = Terminal::new(TestBackend::new(width, 40)).unwrap();
            let mut state = ListState::default();
            term.draw(|f| {
                draw(f, &app, Some(&detail), &mut state);
            })
            .unwrap();
            let out = buffer_string(term.backend().buffer());
            let rows = out.lines().filter(|l| l.contains("a.txt")).count();
            assert_eq!(rows, 1, "{width}\n{out}");
            // The big file's row must hold its count and bar on one row.
            let big = out
                .lines()
                .find(|l| l.contains("413") || l.contains("createDatamorph") || l.contains('…'))
                .unwrap_or_default();
            assert!(big.contains("416"), "{width}: {big:?}\n{out}");
        }
    }

    #[test]
    fn commit_rows_with_wide_characters_do_not_wrap() {
        let app = app_with(vec![row("repo", 1)]);
        let commit = |id: &str| {
            crate::detail::CommitInfo {
            short_id: id.into(),
            summary: "🟢🟢🟢🟢🟢🟢🟢🟢🟢🟢 chore(distribution): upgrade version main-v2.48.0 and more words done"
                .into(),
            author: "Abderrahim ALLOUM (EXT)".into(),
            time: Some(0),
        }
        };
        let detail = RepoDetail {
            recent: vec![commit("aaaaaaa")],
            ..RepoDetail::default()
        };
        for width in 60..=140 {
            let mut term = Terminal::new(TestBackend::new(width, 30)).unwrap();
            let mut state = ListState::default();
            term.draw(|f| {
                draw(f, &app, Some(&detail), &mut state);
            })
            .unwrap();
            let out = buffer_string(term.backend().buffer());
            assert!(
                !out.contains("(EXT)") || out.contains("ALLOUM (EXT)"),
                "{width}\n{out}"
            );
            let at = out.lines().position(|l| l.contains("aaaaaaa")).unwrap();
            let next = out.lines().nth(at + 1).unwrap_or_default();
            assert!(
                !["chore", "…", "(EXT)", "done"]
                    .iter()
                    .any(|w| next.contains(w)),
                "{width}: wrapped\n{out}"
            );
        }
    }

    #[test]
    fn recent_commits_show_their_author() {
        let app = app_with(vec![row("repo", 1)]);
        let detail = RepoDetail {
            recent: vec![crate::detail::CommitInfo {
                short_id: "aaaaaaa".into(),
                summary: "fix things".into(),
                author: "Grace Hopper".into(),
                time: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64
                        - 2 * 3_600
                        - 30,
                ),
            }],
            ..RepoDetail::default()
        };
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut state = ListState::default();
        term.draw(|f| {
            draw(f, &app, Some(&detail), &mut state);
        })
        .unwrap();
        let out = buffer_string(term.backend().buffer());
        let row = out.lines().find(|l| l.contains("aaaaaaa")).unwrap();
        assert!(
            row.ends_with("Grace Hopper │"),
            "author is flush right:\n{out}"
        );
        assert!(out.contains("   2h aaaaaaa fix things"), "{out}");
    }

    #[test]
    fn the_detail_pane_lists_what_is_incoming() {
        let app = app_with(vec![row("repo", 1)]);
        let detail = RepoDetail {
            incoming: Some(Incoming {
                files: vec![FileDelta {
                    added: 42,
                    removed: 17,
                    path: "src/api/users.rs".into(),
                }],
                truncated: false,
                total_added: 42,
                total_removed: 17,
                range: "abc1234..def5678".into(),
                commits: vec![crate::detail::CommitInfo {
                    short_id: "def5678".into(),
                    summary: "add users endpoint".into(),
                    author: "Ada".into(),
                    time: None,
                }],
                total_commits: 1,
            }),
            ..RepoDetail::default()
        };
        let mut term = Terminal::new(TestBackend::new(260, 30)).unwrap();
        let mut state = ListState::default();
        term.draw(|f| {
            draw(f, &app, Some(&detail), &mut state);
        })
        .unwrap();
        let out = buffer_string(term.backend().buffer());

        assert!(out.contains("abc1234..def5678"), "{out}");
        assert!(out.contains("src/api/users.rs"), "{out}");
        assert!(
            out.contains("1 file, 42 insertions(+), 17 deletions(-)"),
            "{out}"
        );
        let row = out
            .lines()
            .find(|l| l.contains("add users endpoint"))
            .unwrap();
        assert!(row.ends_with("Ada │"), "author is flush right:\n{out}");
        assert!(out.contains("Shift+d diff"), "{out}");
    }

    #[test]
    fn the_header_lists_every_navigation_binding_when_it_fits() {
        let app = app_with(vec![row("a", 1)]);
        let out = render(120, 20, &app);
        for hint in HINTS {
            let piece = format!("{} {}", hint.0, hint.1);
            assert!(out.contains(&piece), "missing {piece:?}\n{out}");
        }
    }

    #[test]
    fn a_narrow_terminal_drops_hints_but_keeps_help_and_quit() {
        let app = app_with(vec![row("a", 1)]);
        let out = render(40, 12, &app);
        assert!(out.contains("? help | Esc quit"), "{out}");
        assert!(out.contains("move"), "{out}");
    }

    #[test]
    fn every_footer_hint_is_a_key_the_help_pane_explains() {
        // Rows list key groups like "g / G"; a row whose whole key is "/" is
        // the filter key, not a separator.
        let helped: Vec<&str> = HELP
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .flat_map(|(keys, _)| {
                if *keys == "/" {
                    vec!["/"]
                } else {
                    keys.split_whitespace().filter(|k| *k != "/").collect()
                }
            })
            .collect();
        let tail_keys = TAIL
            .split(" | ")
            .filter_map(|pair| pair.split_whitespace().next());
        let hint_keys = HINTS
            .iter()
            .flat_map(|(key, _)| key.split_whitespace().filter(|k| *k != "/" || *key == "/"));
        for key in hint_keys.chain(tail_keys) {
            assert!(
                helped.contains(&key),
                "the footer offers {key} but the help pane never explains it"
            );
        }
    }
}
