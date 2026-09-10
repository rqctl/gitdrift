use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::detail::{FileDelta, RepoDetail};
use crate::render::{elide, head_label, shorten_path, status_cells, status_width};
use crate::status::RepoStatus;
use crate::theme::{self, Facet};
use crate::ui::state::{App, Job, Pane};

const DETAIL_MIN_WIDTH: u16 = 100;

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

/// Column widths for one frame, sized so the status column always lands on
/// screen: the name is elided to whatever is left over, never the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cols {
    name: usize,
    head: usize,
    /// The band under the selected row has to reach the far edge.
    total: usize,
}

const CURSOR_W: usize = 1;
const MARK_W: usize = 2;
const GAP_W: usize = 2;
/// Branch names are shown whole. The cap only stops one pathological name
/// from swallowing the row.
const HEAD_MAX: usize = 60;
const STATUS_MAX: usize = 40;
const NAME_MIN: usize = 12;
/// Widest the incoming +/- bar is allowed to get.
const BAR_MAX: usize = 24;

impl Cols {
    fn measure(rows: &[&RepoStatus], inner_width: usize) -> Cols {
        let natural_head = rows
            .iter()
            .map(|r| head_label(r).chars().count())
            .max()
            .unwrap_or(6);
        let status = rows
            .iter()
            .map(|r| status_width(r))
            .max()
            .unwrap_or(2)
            .min(STATUS_MAX);
        let longest = rows
            .iter()
            .map(|r| r.display_name.chars().count())
            .max()
            .unwrap_or(NAME_MIN);

        // Branch and status get their full width; the name absorbs the rest
        // and shortens its middle to fit.
        let mut head = natural_head.clamp(1, HEAD_MAX);
        let fixed = CURSOR_W + MARK_W + GAP_W + GAP_W + status;
        let mut name = longest.min(inner_width.saturating_sub(fixed + head));

        if name < NAME_MIN {
            // Too narrow to honour both. Give the name its floor out of the
            // branch column, which then elides.
            name = NAME_MIN.min(longest);
            head = inner_width.saturating_sub(fixed + name).clamp(1, head);
        }
        Cols {
            name,
            head,
            total: inner_width,
        }
    }
}

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
            if marked { "● " } else { "  " },
            Style::default().fg(theme::Color::Accent.to_ratatui()),
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

pub fn draw(
    frame: &mut Frame,
    app: &App,
    detail: Option<&RepoDetail>,
    list_state: &mut ListState,
) -> Rendered {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let show_detail = area.width >= DETAIL_MIN_WIDTH && app.pane() == Pane::List;
    let body = if show_detail {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[0])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(rows[0])
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
        Pane::List => {
            draw_list(frame, app, body[0], list_state);
            if show_detail {
                max_scroll = draw_detail(frame, app, detail, body[1]);
            }
        }
    }
    draw_footer(frame, app, rows[1]);

    Rendered {
        detail_area: show_detail.then(|| body[1]),
        pane_max_scroll: max_scroll,
    }
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, state: &mut ListState) {
    let visible = app.visible();
    let count = if app.is_scanning() {
        format!("Scanning… {} repositories", visible.len())
    } else {
        format!("{} repositories", visible.len())
    };
    let scope = app.group().unwrap_or("all");
    let title = format!(" {count} · {scope} · by {} ", app.sort_mode().label());
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(title);

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

    // Two border columns and the block's one-column padding on each side.
    let cols = Cols::measure(&visible, area.width.saturating_sub(4) as usize);

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
    let list = List::new(items).block(block);
    state.select(Some(app.selected_index()));
    frame.render_stateful_widget(list, area, state);
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
                .padding(Padding::horizontal(1))
                .title(format!(" {title} {arrows}")),
        )
        .scroll((scroll, 0)),
        area,
    );
    max
}

fn draw_detail(frame: &mut Frame, app: &App, detail: Option<&RepoDetail>, area: Rect) -> u16 {
    let block = || {
        Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(" Detail ")
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
        lines.push(Line::from(Span::styled(
            format!("last fetch: {}h ago", age.as_secs() / 3600),
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

    lines.push(section("Branches"));
    for b in &d.branches {
        let mut spans = vec![
            Span::raw(if b.is_head { "* " } else { "  " }),
            Span::raw(b.name.clone()),
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
        if b.upstream.is_none() {
            spans.push(Span::styled(" ⊘", facet_style(Facet::NoUpstream)));
        }
        lines.push(Line::from(spans));
    }

    if let Some(inc) = &d.incoming {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            section_span("Incoming"),
            Span::raw("  "),
            Span::styled(inc.range.clone(), accent()),
        ]));
        let max = inc
            .files
            .iter()
            .map(|f| f.added + f.removed)
            .max()
            .unwrap_or(0);
        let count_w = max.to_string().len();
        // Paths get whatever the pane leaves after the gutter, count and bar.
        let bar_w = BAR_MAX.min(usize::from(area.width).saturating_sub(28));
        let name_w = inc
            .files
            .iter()
            .map(|f| f.path.chars().count())
            .max()
            .unwrap_or(0)
            .min(usize::from(area.width).saturating_sub(count_w + bar_w + 8));
        for f in &inc.files {
            let mut spans = vec![
                Span::raw("  "),
                Span::raw(format!("{:name_w$} ", shorten_path(&f.path, name_w))),
                Span::styled(format!("{:>count_w$} ", f.added + f.removed), dim()),
            ];
            spans.extend(stat_bar(f, max, bar_w));
            lines.push(Line::from(spans));
        }
        if inc.truncated {
            lines.push(Line::from(Span::styled("  …", dim())));
        }
        lines.push(Line::from(Span::styled(
            format!(
                "  {} file{}, {} insertions(+), {} deletions(-)",
                inc.files.len(),
                if inc.files.len() == 1 { "" } else { "s" },
                inc.total_added,
                inc.total_removed
            ),
            dim(),
        )));
        lines.push(Line::from(Span::styled("  D diff   L log", accent())));
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

    if !d.recent.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section("Recent"));
        for c in &d.recent {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", c.short_id), dim()),
                Span::raw(c.summary.clone()),
            ]));
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
        .padding(Padding::horizontal(1))
        .title(" Confirm ");
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
        Span::styled("y", accent()),
        Span::raw(" to proceed · any other key cancels"),
    ]));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
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
        .padding(Padding::horizontal(1))
        .title(format!(" {title} "));
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
        "↑↓ move   Enter switch   Esc close",
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

/// The list-pane keymap, in help-pane order. Also read by the footer/help
/// cross-check test.
const HELP: &[(&str, &str)] = &[
    ("↑↓ PgUp PgDn", "move"),
    ("g / G", "first / last"),
    ("n", "filter by namespace"),
    ("o", "cycle sort: drift, name, recent, stale"),
    ("Space", "mark / unmark (actions apply to marks)"),
    ("V", "sweep the last mark's state to here"),
    ("a", "mark all visible, or clear the marks"),
    ("f / F", "fetch marked-or-current / fetch all"),
    ("p", "pull --ff-only, marked-or-current"),
    ("P", "prune gone branches (confirmed)"),
    ("s / e", "shell / editor in repo"),
    ("D", "diff HEAD...@{u} in your pager"),
    ("L", "log of the incoming commits, in your pager"),
    ("b", "switch branch on the current repo"),
    ("S", "browse stashes: view, drop"),
    ("x", "drop the selected stash (stash pane, confirmed)"),
    ("J / K", "scroll the detail pane"),
    (
        "wheel",
        "scroll the detail under the pointer, else the list",
    ),
    ("r", "rescan"),
    ("d", "drifted only"),
    ("/", "filter"),
    ("!", "problems"),
    ("?", "this help"),
    ("Esc / q", "quit"),
    ("Ctrl-C", "quit"),
];

fn draw_help(frame: &mut Frame, app: &App, area: Rect) -> u16 {
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("  {k:14}"), accent()),
                Span::raw(*v),
            ])
        })
        .collect();
    scrollable(
        frame,
        area,
        "Help (? or Esc to close)",
        lines,
        app.pane_scroll(),
    )
}

/// Every binding worth a reminder, in the order they are dropped when the
/// terminal is too narrow to hold them all.
const HINTS: &[(&str, &str)] = &[
    ("↑↓", "move"),
    ("n", "namespace"),
    ("o", "sort"),
    ("Space", "mark"),
    ("V", "range"),
    ("a", "all"),
    ("f", "fetch"),
    ("p", "pull"),
    ("P", "prune"),
    ("D", "diff"),
    ("L", "log"),
    ("b", "branch"),
    ("S", "stash"),
    ("/", "filter"),
];

/// Pinned at the end of the footer; never dropped for width.
const TAIL: &str = "? help  Esc quit";

/// As many labelled hints as fit. Help and quit are never dropped: a
/// truncated footer must still say how to get out and where the rest is.
fn footer_hints(width: usize) -> String {
    const GAP: usize = 2;
    let mut out = String::new();
    let mut used = 0usize;
    for (key, label) in HINTS {
        let piece = format!("{key} {label}");
        let extra = piece.chars().count() + if used == 0 { 0 } else { GAP };
        if used + extra + GAP + TAIL.len() > width {
            break;
        }
        if used > 0 {
            out.push_str("  ");
        }
        out.push_str(&piece);
        used += extra;
    }
    if used > 0 {
        out.push_str("  ");
    }
    out.push_str(TAIL);
    out
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

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let spans = if let Some(filter) = app.filter_text() {
        vec![
            Span::styled("/", accent()),
            Span::raw(filter.to_string()),
            Span::styled("▏", accent()),
        ]
    } else if let Some(text) = activity(app.jobs(), spinner_frame(since_start())) {
        // Several toasts land while a job is still running, the stash-drop
        // recovery id among them; the spinner must not swallow them.
        match app.toast() {
            Some(toast) => vec![Span::styled(format!("{text}  {toast}"), accent())],
            None => {
                let used = text.chars().count() + 2;
                vec![
                    Span::styled(format!("{text}  "), accent()),
                    Span::styled(footer_hints(width.saturating_sub(used)), dim()),
                ]
            }
        }
    } else if let Some(toast) = app.toast() {
        vec![Span::styled(toast.to_string(), accent())]
    } else {
        vec![Span::styled(footer_hints(width), dim())]
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
    fn narrow_terminals_drop_the_detail_pane_rather_than_squashing_it() {
        let app = app_with(vec![row("sre/alpha", 1)]);
        let wide = render(120, 20, &app);
        let narrow = render(70, 20, &app);
        assert!(
            wide.contains("Detail"),
            "wide layout should show the detail pane:\n{wide}"
        );
        assert!(
            !narrow.contains("Detail"),
            "narrow layout should hide it:\n{narrow}"
        );
    }

    #[test]
    fn the_footer_shows_keybindings() {
        let app = app_with(vec![row("a", 1)]);
        let out = render(120, 20, &app);
        assert!(out.contains("fetch"), "{out}");
        assert!(out.contains("quit"), "{out}");
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

    #[test]
    fn the_activity_line_shows_the_label_and_its_progress() {
        let jobs = [Job {
            id: 1,
            label: "fetching".into(),
            done: 37,
            total: 511,
        }];
        assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ fetching 37/511");
    }

    #[test]
    fn an_indeterminate_job_shows_only_what_it_has_done() {
        let jobs = [Job {
            id: 1,
            label: "scanning".into(),
            done: 214,
            total: 0,
        }];
        assert_eq!(activity(&jobs, '⠹').unwrap(), "⠹ scanning 214");
    }

    #[test]
    fn extra_jobs_are_counted_not_listed() {
        let jobs = [
            Job {
                id: 1,
                label: "fetching".into(),
                done: 1,
                total: 2,
            },
            Job {
                id: 2,
                label: "pruning".into(),
                done: 0,
                total: 4,
            },
        ];
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
        let out = render(120, 24, &app);
        assert!(out.contains("ff-only"), "{out}");
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
            crossterm::event::KeyCode::Char('P'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(120, 24, &app);
        assert!(out.contains("prune remotes"), "{out}");
        assert!(out.contains("cannot be undone"), "{out}");
        assert!(out.contains("y"), "{out}");
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
        assert!(after.contains('●'), "{after}");
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
        let mut term = Terminal::new(TestBackend::new(120, height)).unwrap();
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

        for _ in 0..40 {
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
        term.draw(|f| {
            draw(f, &app, None, &mut state);
        })
        .unwrap();
        let buf = term.backend().buffer();

        let bars: Vec<u16> = (0..buf.area.height)
            .filter(|y| (0..buf.area.width).any(|x| buf[(x, *y)].symbol() == "▌"))
            .collect();
        assert_eq!(bars.len(), 1, "exactly one row carries the cursor");

        let y = bars[0];
        for x in 0..buf.area.width {
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
            (0..buf.area.width).all(|x| buf[(x, y)].fg != theme::Color::Dim.to_ratatui()),
            "dim text would vanish into the band"
        );
        let ahead = (0..buf.area.width)
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
            .filter(|l| l.starts_with('│') && l.contains('↑'))
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
            }),
            ..RepoDetail::default()
        };
        let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
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
        assert!(out.contains("D diff"), "{out}");
    }

    #[test]
    fn every_footer_hint_is_a_key_the_help_pane_explains() {
        // Rows list key groups like "g / G"; a row whose whole key is "/" is
        // the filter key, not a separator.
        let helped: Vec<&str> = HELP
            .iter()
            .flat_map(|(keys, _)| {
                if *keys == "/" {
                    vec!["/"]
                } else {
                    keys.split_whitespace().filter(|k| *k != "/").collect()
                }
            })
            .collect();
        let tail_keys = TAIL
            .split("  ")
            .filter_map(|pair| pair.split_whitespace().next());
        let hint_keys = HINTS.iter().map(|(key, _)| *key);
        for key in hint_keys.chain(tail_keys) {
            assert!(
                helped.contains(&key),
                "the footer offers {key} but the help pane never explains it"
            );
        }
    }
}
